using PrismClient;
using System.Text;

Console.OutputEncoding = Encoding.UTF8;
Console.WriteLine("🚀 Prism .NET Integration Example");
Console.WriteLine("=================================");

// Initialize library
PrismNative.prism_init();
Console.WriteLine("✅ Prism Library Initialized");

// Create a sample buffer (PDF signature)
byte[] pdfData = [0x25, 0x50, 0x44, 0x46, 0x2D, 0x31, 0x2E, 0x35, 0x0A]; // %PDF-1.5\n
Console.WriteLine($"\n📄 Testing with PDF buffer ({pdfData.Length} bytes)...");

// 1. Detect Format
var formatJsonPtr = PrismNative.prism_detect_format(pdfData, (UIntPtr)pdfData.Length);
var formatJson = PrismNative.PtrToStringAndFree(formatJsonPtr);

if (formatJson != null)
{
    Console.WriteLine($"✅ Detected Format JSON:\n{formatJson}");
}
else
{
    Console.WriteLine("❌ Format Detection Failed");
}

// 2. Getting Preview for Unknown/Binary Data
// Create a fake binary buffer
byte[] binaryData = new byte[20];
for (int i = 0; i < 20; i++) binaryData[i] = (byte)(i * 10);
Console.WriteLine($"\n📦 Testing with random binary data ({binaryData.Length} bytes)...");

var previewPtr = PrismNative.prism_preview_file(binaryData, (UIntPtr)binaryData.Length);
var preview = PrismNative.PtrToStringAndFree(previewPtr);

Console.WriteLine($"✅ Preview Content:\n{preview}");

// 3. Testing HTML Conversion
Console.WriteLine($"\n🌍 Testing HTML Conversion (PDF Header)...");
var htmlPtr = PrismNative.prism_convert_to_html(pdfData, (UIntPtr)pdfData.Length);
var html = PrismNative.PtrToStringAndFree(htmlPtr);

if (html != null)
{
    Console.WriteLine($"✅ Generated HTML (First 100 chars):");
    Console.WriteLine(html.Length > 100 ? html.Substring(0, 100) + "..." : html);
}
else
{
    Console.WriteLine("❌ HTML Conversion Failed or Returned Null");
}

Console.WriteLine("\nDone. Press any key to exit.");
Console.ReadLine();
