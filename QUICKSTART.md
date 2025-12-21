# Prism Quick Start Guide

Get up and running with Prism in 5 minutes!

## Prerequisites

- Rust 1.70 or later
- A web browser (Chrome, Firefox, Safari, or Edge)

## Step 1: Build the Project

```bash
cd c:\Dev\RustSandbox\Prism
cargo build --release
```

## Step 2: Start the Server

```bash
cargo run --bin prism-server
```

You should see:

```
INFO Starting Prism Server v0.1.0
INFO Server listening on 127.0.0.1:8080
```

Keep this terminal window open - the server needs to stay running.

## Step 3: Open the Web Viewer

Open a new terminal/command prompt and navigate to the web viewer:

```bash
cd c:\Dev\RustSandbox\Prism\web-viewer
```

Then simply open `index.html` in your browser:

```bash
start index.html
```

Or use a local web server (recommended):

```bash
# Using Python 3
python -m http.server 3000

# Then open http://localhost:3000 in your browser
```

## Step 4: Upload a Test File

The web viewer should now be open in your browser. Try uploading one of the test files:

### Test with a Text File

1. Click "Choose File" in the web viewer
2. Navigate to `c:\Dev\RustSandbox\Prism\test-files\`
3. Select `sample.txt` or `sample.json`
4. Click Open
5. Watch your document render!

### Test with an Excel File

Create a simple Excel file or use any `.xlsx` file you have:

1. Click "Choose File"
2. Select any `.xlsx` file
3. Watch the spreadsheet convert to HTML with all cells displayed

### Test with an Image

1. Click "Choose File"
2. Select any `.png` image
3. The image will be displayed in the viewer

## That's It!

You now have:
- ✅ A running Prism document processing server
- ✅ A web-based viewer interface
- ✅ Support for PNG, XLSX, TXT, JSON, XML, LOG, CSV, MD formats

## What to Try Next

### Test Different Formats

Upload files with these extensions:
- `.txt` - Plain text files
- `.log` - Log files
- `.json` - JSON data
- `.xml` - XML documents
- `.csv` - CSV data
- `.md` - Markdown files
- `.xlsx` - Excel spreadsheets
- `.png` - PNG images

### Use the API Directly

You can also use curl to test the API:

**Windows (PowerShell):**
```powershell
$file = "c:\Dev\RustSandbox\Prism\test-files\sample.txt"
curl.exe -X POST http://localhost:8080/convert -F "file=@$file" -o output.html
start output.html
```

**Windows (Command Prompt):**
```cmd
curl.exe -X POST http://localhost:8080/convert -F "file=@c:\Dev\RustSandbox\Prism\test-files\sample.txt" -o output.html
start output.html
```

### Check Server Health

```bash
curl http://localhost:8080/health
```

Response:
```json
{
  "status": "ok",
  "version": "0.1.0"
}
```

### Check Version Info

```bash
curl http://localhost:8080/version
```

Response:
```json
{
  "server": "0.1.0",
  "core": "0.1.0",
  "parsers": "0.1.0",
  "render": "0.1.0",
  "sandbox": "0.1.0"
}
```

## Troubleshooting

### Server Won't Start

**Error: "Address already in use"**
- Another process is using port 8080
- Stop the other process or change the port in `main.rs`

**Error: "Failed to compile"**
- Run `cargo clean` and try building again
- Check that you have the latest Rust version

### Web Viewer Shows "Server: Offline"

1. Make sure the server is running (Step 2)
2. Check the server terminal for errors
3. Verify the server is on port 8080
4. Try refreshing the web page

### File Upload Fails

**CORS Error:**
- Open the web viewer via a web server (not `file://`)
- Use Python or Node.js as shown in Step 3

**File Too Large:**
- Default limit is 10 MB
- Check server logs for size limit errors

### Nothing Renders

1. Check browser console (F12) for errors
2. Verify the file format is supported
3. Try a different test file
4. Check server logs for parsing errors

## Advanced Usage

### Running Tests

```bash
cargo test
```

Expected output:
```
running 53 tests
test result: ok. 53 passed; 0 failed; 0 ignored
```

### Building for Production

```bash
cargo build --release
```

The optimized binary will be in `target/release/prism-server.exe`

### Viewing Logs

The server logs to stdout. To save logs to a file:

```bash
cargo run --bin prism-server > server.log 2>&1
```

## Next Steps

- Explore the codebase in `crates/`
- Read the full documentation in `README.md`
- Try implementing a new parser for a different format
- Customize the web viewer styling
- Add more test files

## Getting Help

- Check the main README.md for detailed documentation
- Review the web-viewer/README.md for viewer-specific info
- Look at test files in `crates/*/tests/`
- Examine the code in `crates/prism-parsers/src/`

Enjoy using Prism! 🎉
