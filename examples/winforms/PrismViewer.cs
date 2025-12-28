using System;
using System.IO;
using System.Windows.Forms;
using Microsoft.Web.WebView2.Core;
using PrismClient; // Reference to your PrismNative static class

namespace Prism.WinForms
{
    public partial class PrismViewer : UserControl
    {
        private Microsoft.Web.WebView2.WinForms.WebView2 webView;

        public PrismViewer()
        {
            InitializeComponent();
            InitializeAsync();
        }

        private async void InitializeAsync()
        {
            // Initialize WebView2
            webView = new Microsoft.Web.WebView2.WinForms.WebView2();
            webView.Dock = DockStyle.Fill;
            this.Controls.Add(webView);
            await webView.EnsureCoreWebView2Async();
            PrismNative.prism_init(); // Initialize Prism Library if not already done
        }

        public void LoadFile(string filePath)
        {
            if (!File.Exists(filePath))
                throw new FileNotFoundException("File not found", filePath);

            try
            {
                byte[] data = File.ReadAllBytes(filePath);
                
                // Call Prism to convert to HTML
                IntPtr htmlPtr = PrismNative.prism_convert_to_html(data, (UIntPtr)data.Length);
                string html = PrismNative.PtrToStringAndFree(htmlPtr);

                if (string.IsNullOrEmpty(html))
                {
                    // Fallback to preview if conversion failed
                    IntPtr previewPtr = PrismNative.prism_preview_file(data, (UIntPtr)data.Length);
                    string preview = PrismNative.PtrToStringAndFree(previewPtr);
                    html = $"<html><body><h2>Conversion Failed</h2><pre>{System.Web.HttpUtility.HtmlEncode(preview)}</pre></body></html>";
                }

                // Render HTML in WebView2
                if (webView != null && webView.CoreWebView2 != null)
                {
                    webView.NavigateToString(html);
                }
            }
            catch (Exception ex)
            {
                MessageBox.Show($"Error loading file: {ex.Message}", "Prism Viewer Error", MessageBoxButtons.OK, MessageBoxIcon.Error);
            }
        }

        private void InitializeComponent()
        {
            this.AutoScaleMode = System.Windows.Forms.AutoScaleMode.Font;
        }
    }
}
