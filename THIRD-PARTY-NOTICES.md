# Third-party software bundled with Zavyx DM

The Zavyx DM installer and portable ZIP include these separate programs. Zavyx DM runs
them as external tools; they keep their own licences.

## yt-dlp
- Used for: downloading videos from YouTube and other sites
- Licence: The Unlicense (public domain)
- Source: https://github.com/yt-dlp/yt-dlp

## FFmpeg
- Used for: merging video + audio, and saving HLS/DASH streams as MP4
- Build: "essentials" build by Gyan Doshi — https://www.gyan.dev/ffmpeg/builds/
- Licence: GNU GPL v3 — https://www.gnu.org/licenses/gpl-3.0.html
- Source code: https://ffmpeg.org/download.html and https://github.com/GyanD/codexffmpeg

Zavyx DM itself is released under the MIT licence (see `LICENSE`). Rust crate
dependencies are listed in `Cargo.lock` with their own licences.
