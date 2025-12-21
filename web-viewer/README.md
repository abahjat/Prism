# Prism Document Viewer - Web Interface

A simple, elegant web-based viewer for the Prism Document Processing SDK. Upload and view documents in your browser!

## Features

- 📁 **Drag & Drop Support** - Simply drag files onto the upload area
- 🎨 **Beautiful UI** - Modern, responsive design
- 🚀 **Fast Processing** - Real-time document conversion
- 📱 **Mobile Friendly** - Works on all devices
- ✅ **Format Detection** - Automatic format detection with fallback

## Supported Formats

- **Images**: PNG
- **Spreadsheets**: XLSX (Excel)
- **Text Files**: TXT, LOG, JSON, XML, CSV, MD (Markdown)

## Quick Start

### 1. Start the Prism Server

First, make sure the Prism server is running:

```bash
cd c:\Dev\RustSandbox\Prism
cargo run --bin prism-server
```

The server will start on `http://localhost:8080`

### 2. Open the Web Viewer

Simply open `index.html` in your web browser:

```bash
# Option 1: Double-click index.html in File Explorer

# Option 2: Open from command line
start index.html
```

Or use a local web server (recommended):

```bash
# Using Python 3
python -m http.server 3000

# Using Node.js (if you have http-server installed)
npx http-server -p 3000

# Then open http://localhost:3000 in your browser
```

### 3. Upload a File

1. Click "Choose File" or drag and drop a supported file
2. Wait for processing (usually instant)
3. View your document!

## Usage Examples

### Viewing a Text File

1. Upload any `.txt`, `.log`, `.json`, or `.md` file
2. The content will be displayed with proper formatting and line wrapping
3. Code-friendly monospace font for readability

### Viewing a Spreadsheet

1. Upload an `.xlsx` Excel file
2. Each worksheet becomes a separate page
3. Cell data is displayed in a structured table format

### Viewing an Image

1. Upload a `.png` image
2. The image will be displayed at full resolution
3. Proper scaling and quality preservation

## File Size Limits

By default, the server accepts files up to:
- **Default**: 10 MB
- **Maximum**: 100 MB (configurable in server settings)

## Troubleshooting

### Server Offline

If you see "Server: Offline ✗" in the footer:

1. Make sure the Prism server is running on port 8080
2. Check the terminal for any error messages
3. Try restarting the server

### CORS Errors

If you see CORS errors in the browser console:

1. The server is already configured with permissive CORS
2. Make sure you're accessing the web viewer via a proper URL (not `file://`)
3. Use a local web server as shown in Quick Start

### File Not Rendering

If a file uploads but doesn't render:

1. Check the browser console for errors
2. Verify the file format is supported
3. Try a different file to isolate the issue

## Architecture

```
┌─────────────┐         ┌─────────────┐         ┌──────────────┐
│   Browser   │ ─HTTP─> │ Prism Server│ ─Parse─>│   Renderer   │
│  (Viewer)   │ <─HTML──│  (Port 8080)│ <─HTML──│  (HTML5)     │
└─────────────┘         └─────────────┘         └──────────────┘
```

1. **Upload**: Browser sends file via multipart/form-data
2. **Detection**: Server detects file format
3. **Parsing**: Appropriate parser converts to UDM
4. **Rendering**: HTML renderer creates viewable output
5. **Display**: Browser displays in iframe

## API Endpoints Used

- `GET /health` - Server health check
- `POST /convert` - Convert and render document
- `GET /version` - Server version info (not used by viewer)

## Customization

### Changing Server URL

Edit `app.js` line 3:

```javascript
const API_BASE_URL = 'http://localhost:8080';
```

### Styling

Modify `styles.css` to customize colors, fonts, and layout:

- Color scheme: Update gradient colors in `body` and `.btn-primary`
- Fonts: Change `font-family` in `body`
- Layout: Adjust `.container` max-width

## Security Notes

⚠️ **This is a demo application**

- Do not expose this viewer to the public internet without authentication
- The server should only be accessible from localhost or trusted networks
- Validate and sanitize file inputs in production environments
- Consider implementing file type restrictions based on your needs

## Browser Compatibility

- ✅ Chrome/Edge 90+
- ✅ Firefox 88+
- ✅ Safari 14+
- ✅ Opera 76+

## Development

To modify the viewer:

1. Edit HTML structure in `index.html`
2. Update styles in `styles.css`
3. Modify behavior in `app.js`
4. Refresh browser to see changes

No build process required - it's pure HTML/CSS/JavaScript!

## Future Enhancements

Planned features:

- [ ] Multi-file upload
- [ ] Download converted documents
- [ ] Print functionality
- [ ] Dark mode toggle
- [ ] File history
- [ ] Advanced viewing options (zoom, pagination)
- [ ] Format-specific rendering options

## License

Part of the Prism Document Processing SDK
