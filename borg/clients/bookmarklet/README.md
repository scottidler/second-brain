# obsidian-borg Bookmarklet

One-click URL capture from any browser tab.

## Setup

1. Copy the snippet below
2. Create a new bookmark in your browser
3. Paste the snippet as the bookmark URL
4. Edit `localhost:8181` if your daemon uses a different host/port

## Bookmarklet

```javascript
javascript:void(fetch('http://localhost:8181/ingest',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({url:location.href})}).then(r=>r.json()).then(d=>alert(d.title||'Sent!')).catch(e=>alert('Error: '+e)))
```

## Usage

1. Navigate to the page you want to capture
2. Click the bookmarklet in your bookmarks bar
3. An alert shows the captured title or an error

## Notes

- The bookmarklet runs in the page's origin context, so the daemon must have CORS enabled (it does by default)
- Chrome allows `http://localhost` from HTTPS pages as a special case
- Firefox may block mixed content (HTTPS page -> HTTP localhost) in some configurations
- For more reliable desktop capture, use the [WebExtension](../extension/) instead
