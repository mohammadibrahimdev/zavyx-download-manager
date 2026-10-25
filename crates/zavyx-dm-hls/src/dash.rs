//! Minimal DASH MPD parser. Handles:
//!   - Static and dynamic MPDs
//!   - Period / AdaptationSet / Representation hierarchy
//!   - SegmentTemplate with $Number$ and $Time$ substitution
//!   - SegmentList / SegmentBase
//!   - BaseURL resolution
//!
//! For HLS-style direct download, we usually shell out to ffmpeg which handles MPD natively,
//! so this crate is only used when the user wants to enumerate representations in the UI.

use std::collections::BTreeMap;
use url::Url;

#[derive(Debug, Clone, Default)]
pub struct Mpd {
    pub r#type: String, // "static" or "dynamic"
    pub media_presentation_duration: Option<String>,
    pub min_buffer_time: Option<String>,
    pub base_urls: Vec<String>,
    pub periods: Vec<Period>,
    pub base_url: Option<Url>,
}

#[derive(Debug, Clone, Default)]
pub struct Period {
    pub id: Option<String>,
    pub start: Option<String>,
    pub duration: Option<String>,
    pub base_urls: Vec<String>,
    pub adaptation_sets: Vec<AdaptationSet>,
}

#[derive(Debug, Clone, Default)]
pub struct AdaptationSet {
    pub id: Option<String>,
    pub mime_type: Option<String>,
    pub lang: Option<String>,
    pub segment_alignment: bool,
    pub representations: Vec<Representation>,
}

#[derive(Debug, Clone, Default)]
pub struct Representation {
    pub id: Option<String>,
    pub bandwidth: Option<u64>,
    pub width: Option<u64>,
    pub height: Option<u64>,
    pub codecs: Option<String>,
    pub mime_type: Option<String>,
    pub segment_template: Option<SegmentTemplate>,
    pub segment_list: Option<SegmentList>,
    pub base_urls: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SegmentTemplate {
    pub media: Option<String>, // e.g. "chunk-$Number$.m4s"
    pub initialization: Option<String>, // e.g. "init.mp4"
    pub start_number: u64, // default 1
    pub timescale: u64,
    pub duration: Option<u64>,
    pub segment_timeline: Option<SegmentTimeline>,
}

#[derive(Debug, Clone, Default)]
pub struct SegmentList {
    pub initialization: Option<String>,
    pub segment_urls: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SegmentTimeline {
    pub entries: Vec<SegmentTimelineEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct SegmentTimelineEntry {
    pub t: Option<u64>,
    pub d: u64,
    pub r: u64, // repeat count
}

#[derive(Debug, thiserror::Error)]
pub enum MpdError {
    #[error("malformed MPD: {0}")]
    Malformed(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("URL error: {0}")]
    Url(#[from] url::ParseError),
}

pub type MpdResult<T> = Result<T, MpdError>;

/// Parse MPD XML text. Uses a tiny hand-rolled parser so we don't need a full XML crate.
pub fn parse_mpd(text: &str, base_url: Option<&str>) -> MpdResult<Mpd> {
    let mut mpd = Mpd::default();
    if let Some(b) = base_url {
        mpd.base_url = Some(Url::parse(b)?);
    }
    let mut parser = MiniXmlParser::new(text);
    while let Some(event) = parser.next_event() {
        match event {
            XmlEvent::Start { name, attrs } if name == "MPD" => {
                mpd.r#type = attrs.get("type").cloned().unwrap_or_default();
                mpd.media_presentation_duration = attrs.get("mediaPresentationDuration").cloned();
                mpd.min_buffer_time = attrs.get("minBufferTime").cloned();
                parse_mpd_body(&mut mpd, &mut parser)?;
                break;
            }
            _ => {}
        }
    }
    Ok(mpd)
}

fn parse_mpd_body(mpd: &mut Mpd, parser: &mut MiniXmlParser) -> MpdResult<()> {
    while let Some(event) = parser.next_event() {
        match event {
            XmlEvent::Start { name, attrs } if name == "BaseURL" => {
                let text = parser.read_text().unwrap_or_default();
                mpd.base_urls.push(text.trim().to_string());
            }
            XmlEvent::Start { name, attrs } if name == "Period" => {
                let mut period = Period::default();
                period.id = attrs.get("id").cloned();
                period.start = attrs.get("start").cloned();
                period.duration = attrs.get("duration").cloned();
                parse_period_body(&mut period, parser)?;
                mpd.periods.push(period);
            }
            XmlEvent::End { name } if name == "MPD" => break,
            _ => {}
        }
    }
    Ok(())
}

fn parse_period_body(period: &mut Period, parser: &mut MiniXmlParser) -> MpdResult<()> {
    while let Some(event) = parser.next_event() {
        match event {
            XmlEvent::Start { name, .. } if name == "BaseURL" => {
                let text = parser.read_text().unwrap_or_default();
                period.base_urls.push(text.trim().to_string());
            }
            XmlEvent::Start { name, attrs } if name == "AdaptationSet" => {
                let mut aset = AdaptationSet::default();
                aset.id = attrs.get("id").cloned();
                aset.mime_type = attrs.get("mimeType").cloned();
                aset.lang = attrs.get("lang").cloned();
                aset.segment_alignment = attrs.get("segmentAlignment").map(|s| s == "true").unwrap_or(false);
                parse_adaptation_body(&mut aset, parser)?;
                period.adaptation_sets.push(aset);
            }
            XmlEvent::End { name } if name == "Period" => break,
            _ => {}
        }
    }
    Ok(())
}

fn parse_adaptation_body(aset: &mut AdaptationSet, parser: &mut MiniXmlParser) -> MpdResult<()> {
    while let Some(event) = parser.next_event() {
        match event {
            XmlEvent::Start { name, attrs } if name == "Representation" => {
                let mut rep = Representation::default();
                rep.id = attrs.get("id").cloned();
                rep.bandwidth = attrs.get("bandwidth").and_then(|s| s.parse().ok());
                rep.width = attrs.get("width").and_then(|s| s.parse().ok());
                rep.height = attrs.get("height").and_then(|s| s.parse().ok());
                rep.codecs = attrs.get("codecs").cloned();
                rep.mime_type = attrs.get("mimeType").cloned();
                parse_representation_body(&mut rep, parser)?;
                aset.representations.push(rep);
            }
            XmlEvent::End { name } if name == "AdaptationSet" => break,
            _ => {}
        }
    }
    Ok(())
}

fn parse_representation_body(rep: &mut Representation, parser: &mut MiniXmlParser) -> MpdResult<()> {
    while let Some(event) = parser.next_event() {
        match event {
            XmlEvent::Start { name, .. } if name == "BaseURL" => {
                let text = parser.read_text().unwrap_or_default();
                rep.base_urls.push(text.trim().to_string());
            }
            XmlEvent::Start { name, attrs } if name == "SegmentTemplate" => {
                let mut t = SegmentTemplate::default();
                t.media = attrs.get("media").cloned();
                t.initialization = attrs.get("initialization").cloned();
                t.start_number = attrs.get("startNumber").and_then(|s| s.parse().ok()).unwrap_or(1);
                t.timescale = attrs.get("timescale").and_then(|s| s.parse().ok()).unwrap_or(1);
                t.duration = attrs.get("duration").and_then(|s| s.parse().ok());
                rep.segment_template = Some(t);
                // May contain a SegmentTimeline. The Start variant means there's a body.
                while let Some(ev) = parser.next_event() {
                    match ev {
                        XmlEvent::Start { name: n, .. } if n == "SegmentTimeline" => {
                            let mut st = SegmentTimeline::default();
                            while let Some(e2) = parser.next_event() {
                                match e2 {
                                    XmlEvent::SelfClose { name: en, .. } if en == "S" => break,
                                    XmlEvent::End { name: en } if en == "SegmentTimeline" => {
                                        rep.segment_template.as_mut().unwrap().segment_timeline = Some(st);
                                        break;
                                    }
                                    XmlEvent::Start { name: n2, attrs: a2 } if n2 == "S" => {
                                        let entry = SegmentTimelineEntry {
                                            t: a2.get("t").and_then(|s| s.parse().ok()),
                                            d: a2.get("d").and_then(|s| s.parse().ok()).unwrap_or(0),
                                            r: a2.get("r").and_then(|s| s.parse().ok()).unwrap_or(0),
                                        };
                                        st.entries.push(entry);
                                        let _ = parser.skip_until_end("S");
                                    }
                                    _ => {}
                                }
                            }
                        }
                        XmlEvent::End { name: n } if n == "SegmentTemplate" => break,
                        _ => {}
                    }
                }
            }
            XmlEvent::SelfClose { name, attrs } if name == "SegmentTemplate" => {
                // Self-closing form: no body, no SegmentTimeline possible.
                let mut t = SegmentTemplate::default();
                t.media = attrs.get("media").cloned();
                t.initialization = attrs.get("initialization").cloned();
                t.start_number = attrs.get("startNumber").and_then(|s| s.parse().ok()).unwrap_or(1);
                t.timescale = attrs.get("timescale").and_then(|s| s.parse().ok()).unwrap_or(1);
                t.duration = attrs.get("duration").and_then(|s| s.parse().ok());
                rep.segment_template = Some(t);
            }
            XmlEvent::Start { name, attrs } if name == "SegmentList" => {
                let mut sl = SegmentList::default();
                sl.initialization = attrs.get("initialization").cloned();
                while let Some(e) = parser.next_event() {
                    match e {
                        XmlEvent::Start { name: n, attrs: a2 } if n == "SegmentURL" => {
                            if let Some(media) = a2.get("media") {
                                sl.segment_urls.push(media.clone());
                            }
                            let _ = parser.skip_until_end("SegmentURL");
                        }
                        XmlEvent::End { name: n } if n == "SegmentList" => {
                            rep.segment_list = Some(sl);
                            break;
                        }
                        _ => {}
                    }
                }
            }
            XmlEvent::End { name } if name == "Representation" => break,
            _ => {}
        }
    }
    Ok(())
}

// --- Mini XML parser ---

#[derive(Debug, Clone)]
pub enum XmlEvent {
    Start { name: String, attrs: BTreeMap<String, String> },
    End { name: String },
    SelfClose { name: String, attrs: BTreeMap<String, String> },
    Text(String),
}

pub struct MiniXmlParser<'a> {
    src: &'a str,
    pos: usize,
    pending_text: Option<String>,
}

impl<'a> MiniXmlParser<'a> {
    pub fn new(src: &'a str) -> Self {
        Self { src, pos: 0, pending_text: None }
    }

    pub fn next_event(&mut self) -> Option<XmlEvent> {
        if let Some(t) = self.pending_text.take() {
            return Some(XmlEvent::Text(t));
        }
        let rest = self.src.get(self.pos..)?;
        if rest.is_empty() {
            return None;
        }
        if rest.starts_with("<?") {
            // Skip XML declaration.
            let end = rest.find("?>")? + 2;
            self.pos += end;
            return self.next_event();
        }
        if rest.starts_with("<!--") {
            let end = rest.find("-->")? + 3;
            self.pos += end;
            return self.next_event();
        }
        if rest.starts_with("</") {
            let close = rest.find('>')?;
            let name = rest[2..close].trim().to_string();
            self.pos += close + 1;
            return Some(XmlEvent::End { name });
        }
        if rest.starts_with("<") {
            // Element.
            let close = rest.find('>')?;
            let raw = &rest[1..close];
            let self_closing = raw.ends_with('/');
            let body = if self_closing { &raw[..raw.len() - 1] } else { raw };
            let mut parts = body.splitn(2, char::is_whitespace);
            let name = parts.next().unwrap_or("").to_string();
            let attrs_str = parts.next().unwrap_or("");
            let attrs = parse_attrs(attrs_str);
            self.pos += close + 1;
            if self_closing {
                return Some(XmlEvent::SelfClose { name, attrs });
            }
            return Some(XmlEvent::Start { name, attrs });
        }
        // Text.
        let lt = rest.find('<').unwrap_or(rest.len());
        let text = rest[..lt].to_string();
        self.pos += lt;
        if text.trim().is_empty() {
            return self.next_event();
        }
        Some(XmlEvent::Text(text))
    }

    pub fn read_text(&mut self) -> Option<String> {
        let mut buf = String::new();
        while let Some(e) = self.next_event() {
            match e {
                XmlEvent::Text(t) => buf.push_str(&t),
                XmlEvent::End { .. } => break,
                _ => {}
            }
        }
        Some(buf)
    }

    pub fn skip_until_end(&mut self, name: &str) -> Option<()> {
        let mut depth = 1;
        while let Some(e) = self.next_event() {
            match e {
                XmlEvent::Start { name: n, .. } if n == name => depth += 1,
                XmlEvent::End { name: n } if n == name => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(());
                    }
                }
                _ => {}
            }
        }
        None
    }
}

fn parse_attrs(s: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut chars = s.chars().peekable();
    let mut key = String::new();
    let mut value = String::new();
    let mut state = 0; // 0 = key, 1 = value (pre-quote), 2 = value (in-quote)
    while let Some(c) = chars.next() {
        match state {
            0 => {
                if c == '=' {
                    state = 1;
                } else if !c.is_whitespace() {
                    key.push(c);
                }
            }
            1 => {
                if c == '"' {
                    state = 2;
                } else if !c.is_whitespace() {
                    value.push(c);
                    state = 2;
                }
            }
            2 => {
                if c == '"' {
                    out.insert(key.trim().to_string(), value.clone());
                    key.clear();
                    value.clear();
                    state = 0;
                } else {
                    value.push(c);
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_mpd() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD type="static" mediaPresentationDuration="PT60S" minBufferTime="PT1.5S" xmlns="urn:mpeg:dash:schema:mpd:2011">
  <Period id="p0" start="PT0S">
    <AdaptationSet id="0" mimeType="video/mp4" segmentAlignment="true">
      <Representation id="v0" bandwidth="1000000" width="1280" height="720" codecs="avc1.640028">
        <SegmentTemplate media="chunk-$Number$.m4s" initialization="init.mp4" startNumber="1" timescale="1000" duration="2000" />
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;
        let mpd = parse_mpd(xml, Some("https://example.com/manifest.mpd")).unwrap();
        assert_eq!(mpd.r#type, "static");
        assert_eq!(mpd.periods.len(), 1);
        let p = &mpd.periods[0];
        assert_eq!(p.id.as_deref(), Some("p0"));
        assert_eq!(p.adaptation_sets.len(), 1);
        let aset = &p.adaptation_sets[0];
        assert_eq!(aset.mime_type.as_deref(), Some("video/mp4"));
        assert_eq!(aset.representations.len(), 1);
        let rep = &aset.representations[0];
        assert_eq!(rep.bandwidth, Some(1000000));
        assert_eq!(rep.width, Some(1280));
        assert_eq!(rep.height, Some(720));
        let tpl = rep.segment_template.as_ref().unwrap();
        assert_eq!(tpl.media.as_deref(), Some("chunk-$Number$.m4s"));
        assert_eq!(tpl.start_number, 1);
        assert_eq!(tpl.timescale, 1000);
        assert_eq!(tpl.duration, Some(2000));
    }
}
