@echo off
REM Unregister the Zavyx DM Native Messaging host.
REM Removes registry keys and manifest files for all Chromium browsers + Firefox.

setlocal

echo Removing Zavyx DM Native Messaging registrations...

reg delete "HKCU\Software\Google\Chrome\NativeMessagingHosts\io.github.mohammadibrahimdev.zavyx_dm" /f 2>nul
reg delete "HKCU\Software\Microsoft\Edge\NativeMessagingHosts\io.github.mohammadibrahimdev.zavyx_dm" /f 2>nul
reg delete "HKCU\Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\io.github.mohammadibrahimdev.zavyx_dm" /f 2>nul
reg delete "HKCU\Software\Vivaldi\Vivaldi\NativeMessagingHosts\io.github.mohammadibrahimdev.zavyx_dm" /f 2>nul
reg delete "HKCU\Software\OperaSoftware\Opera\NativeMessagingHosts\io.github.mohammadibrahimdev.zavyx_dm" /f 2>nul
reg delete "HKCU\Software\Mozilla\NativeMessagingHosts\io.github.mohammadibrahimdev.zavyx_dm" /f 2>nul

del "%LOCALAPPDATA%\Zavyx DM\io.github.mohammadibrahimdev.zavyx_dm.json" 2>nul
del "%APPDATA%\mozilla\native-messaging-hosts\io.github.mohammadibrahimdev.zavyx_dm.json" 2>nul

echo Done.
endlocal
