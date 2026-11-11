# FocalDesk Browser

Install this unpacked extension in Chrome, Edge, or Chromium. Firefox uses the same files after changing `browser` to `chrome` compatibility as needed. It has no broad host permission: it sends only a user-invoked selection or page title/URL to the local native bridge.

Build and install `focaldesk-browser-bridge`, then register `native-host/org.focaldesk.browser.json` for each browser after replacing the extension ID. The bridge bounds message/page text size and forwards requests only to the local FocalDesk AI socket.
