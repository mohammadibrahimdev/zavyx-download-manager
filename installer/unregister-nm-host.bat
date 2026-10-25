@echo off
REM Unregister the Zavyx Native Messaging host.
REM Removes registry keys and manifest files for all Chromium browsers + Firefox.

setlocal

echo Removing Zavyx Native Messaging registrations...

reg delete "HKCU\Software\Google\Chrome\NativeMessagingHosts\io.github.mohammadibrahimdev.zavyx" /f 2>nul
reg delete "HKCU\Software\Microsoft\Edge\NativeMessagingHosts\io.github.mohammadibrahimdev.zavyx" /f 2>nul
reg delete "HKCU\Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\io.github.mohammadibrahimdev.zavyx" /f 2>nul
reg delete "HKCU\Software\Vivaldi\Vivaldi\NativeMessagingHosts\io.github.mohammadibrahimdev.zavyx" /f 2>nul
reg delete "HKCU\Software\OperaSoftware\Opera\NativeMessagingHosts\io.github.mohammadibrahimdev.zavyx" /f 2>nul
reg delete "HKCU\Software\Mozilla\NativeMessagingHosts\io.github.mohammadibrahimdev.zavyx" /f 2>nul

del "%LOCALAPPDATA%\Zavyx\io.github.mohammadibrahimdev.zavyx.json" 2>nul
del "%APPDATA%\mozilla\native-messaging-hosts\io.github.mohammadibrahimdev.zavyx.json" 2>nul

echo Done.
endlocal
