using System;
using System.IO;
using System.Text;
using System.Threading.Tasks;
using System.Windows.Forms;
using Microsoft.Web.WebView2.Core;
using Prism.Native;

namespace Prism.WinForms
{
    /// <summary>
    /// Displays documents rendered by Prism in a locked-down WebView2.
    /// </summary>
    /// <remarks>
    /// Supports both the positioned <see cref="PrismDocument"/> layout viewer
    /// (with SVG pages, selectable text layer, search, and PDF/TIFF export) and
    /// sanitized HTML rendering for email/HTML/EPUB/Markdown documents.
    /// The rendered page is written to a private temporary folder and served
    /// through a WebView2 virtual host. This avoids the 2 MB limit of
    /// <c>NavigateToString</c>. The view cannot navigate anywhere else or open
    /// new windows, and the folder is deleted when the control is disposed.
    /// </remarks>
    public partial class PrismViewer : UserControl
    {
        private const string VirtualHost = "prism-viewer.local";
        private const string PageName = "document.html";

        private readonly Microsoft.Web.WebView2.WinForms.WebView2 webView;
        private readonly string viewDirectory =
            Path.Combine(Path.GetTempPath(), "PrismViewer", Guid.NewGuid().ToString("N"));
        private readonly Task initialization;
        private PrismDocument? activeDocument;

        public PrismViewer()
        {
            InitializeComponent();
            webView = new Microsoft.Web.WebView2.WinForms.WebView2 { Dock = DockStyle.Fill };
            Controls.Add(webView);
            initialization = InitializeAsync();
        }

        /// <summary>Currently opened <see cref="PrismDocument"/>, if loaded via the layout engine.</summary>
        public PrismDocument? ActiveDocument => activeDocument;

        private async Task InitializeAsync()
        {
            PrismLib.prism_init();
            Directory.CreateDirectory(viewDirectory);
            await webView.EnsureCoreWebView2Async();

            var core = webView.CoreWebView2;
            core.SetVirtualHostNameToFolderMapping(
                VirtualHost, viewDirectory, CoreWebView2HostResourceAccessKind.Deny);
            core.NavigationStarting += (_, e) =>
            {
                if (!e.Uri.StartsWith($"https://{VirtualHost}/", StringComparison.OrdinalIgnoreCase))
                    e.Cancel = true;
            };
            core.NewWindowRequested += (_, e) => e.Handled = true;
            core.Settings.AreDevToolsEnabled = false;
            core.Settings.IsStatusBarEnabled = true; // shows neutralized link targets on hover
        }

        public async Task LoadFileAsync(string filePath, bool useLayoutViewer = true)
        {
            if (!File.Exists(filePath))
                throw new FileNotFoundException("File not found", filePath);

            await initialization;

            activeDocument?.Dispose();
            activeDocument = null;

            byte[] data = File.ReadAllBytes(filePath);
            string fileName = Path.GetFileName(filePath);

            string html;
            try
            {
                // The format is detected from content, not the extension; formats the layout
                // engine can't position yet (email, HTML, EPUB, ...) use the sanitized HTML path.
                html = (useLayoutViewer ? TryRenderLayoutViewer(data, fileName) : null)
                       ?? PrismLib.ConvertToHtml(data, fileName);
            }
            catch (PrismException ex)
            {
                // Fall back to a text or hex preview
                string preview = PrismLib.PtrToStringAndFree(
                    PrismLib.prism_preview_file(data, (UIntPtr)data.Length)) ?? string.Empty;
                html = "<!DOCTYPE html><html><head><meta charset=\"utf-8\">" +
                       "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'\">" +
                       "</head><body><h2>Conversion failed</h2>" +
                       $"<p>{System.Net.WebUtility.HtmlEncode(ex.Message)}</p>" +
                       $"<pre>{System.Net.WebUtility.HtmlEncode(preview)}</pre></body></html>";
            }

            File.WriteAllText(Path.Combine(viewDirectory, PageName), html, new UTF8Encoding(false));
            webView.CoreWebView2.Navigate($"https://{VirtualHost}/{PageName}");
        }

        /// <summary>
        /// Opens the document with the layout engine and returns the viewer HTML, or null when
        /// the layout engine can't handle the document.
        /// </summary>
        private string? TryRenderLayoutViewer(byte[] data, string fileName)
        {
            PrismDocument? document = null;
            try
            {
                document = PrismDocument.Open(data, fileName);
                string html = Encoding.UTF8.GetString(document.Export(PrismExportFormat.HtmlViewer));
                activeDocument = document;
                return html;
            }
            catch (PrismException)
            {
                document?.Dispose();
                return null;
            }
        }

        /// <summary>Export the currently loaded document to a searchable PDF file.</summary>
        public void ExportPdf(string outputPath)
        {
            if (activeDocument == null)
                throw new InvalidOperationException("No layout document is currently open.");
            File.WriteAllBytes(outputPath, activeDocument.Export(PrismExportFormat.Pdf));
        }

        /// <summary>Export the currently loaded document to a multi-page TIFF file.</summary>
        public void ExportTiff(string outputPath)
        {
            if (activeDocument == null)
                throw new InvalidOperationException("No layout document is currently open.");
            File.WriteAllBytes(outputPath, activeDocument.Export(PrismExportFormat.Tiff));
        }

        protected override void Dispose(bool disposing)
        {
            if (disposing)
            {
                activeDocument?.Dispose();
                activeDocument = null;
                webView.Dispose();
                try
                {
                    Directory.Delete(viewDirectory, recursive: true);
                }
                catch (IOException)
                {
                    // Still in use; the OS temp cleanup will remove it
                }
            }
            base.Dispose(disposing);
        }

        private void InitializeComponent()
        {
            this.AutoScaleMode = System.Windows.Forms.AutoScaleMode.Font;
        }
    }
}
