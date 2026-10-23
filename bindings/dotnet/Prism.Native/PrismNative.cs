// SPDX-License-Identifier: AGPL-3.0-only
using System;
using System.Runtime.InteropServices;
using System.Text;

namespace Prism.Native;

/// <summary>Error reported by the native Prism library.</summary>
public sealed class PrismException : Exception
{
    public PrismException(string message) : base(message) { }
}

/// <summary>Page render format for <see cref="PrismDocument.RenderPage"/>.</summary>
public enum PrismPageFormat : uint
{
    Svg = 0,
    Png = 1,
    ViewerFragment = 2,
}

/// <summary>Whole-document export format for <see cref="PrismDocument.Export"/>.</summary>
public enum PrismExportFormat : uint
{
    Pdf = 0,
    Tiff = 1,
    HtmlViewer = 2,
}

/// <summary>Bitflags for <see cref="PrismDocument.SearchJson"/>.</summary>
[Flags]
public enum PrismSearchFlags : uint
{
    None = 0,
    CaseSensitive = 1 << 0,
    IgnoreDiacritics = 1 << 1,
    WholeWord = 1 << 2,
}

public static class PrismLib
{
    private const string DllName = "prism_bindings";

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern void prism_init();

    /// <summary>Static string; do not free.</summary>
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr prism_version();

    /// <summary>Message for the last failed call on this thread (static; do not free), or null.</summary>
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr prism_last_error();

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr prism_detect_format(byte[] data, UIntPtr len);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr prism_detect_format_ex(byte[] data, UIntPtr len, byte[]? filename);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr prism_preview_file(byte[] data, UIntPtr len);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr prism_convert_to_html(byte[] data, UIntPtr len);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr prism_convert_to_html_ex(byte[] data, UIntPtr len, byte[]? filename);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr prism_extract_text(byte[] data, UIntPtr len, byte[]? filename);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr prism_extract_metadata(byte[] data, UIntPtr len, byte[]? filename);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern void prism_free_string(IntPtr ptr);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern void prism_buffer_free(IntPtr buf, UIntPtr len);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr prism_document_open(byte[] data, UIntPtr len, byte[]? filename);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern void prism_document_close(IntPtr doc);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern uint prism_document_page_count(IntPtr doc);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern int prism_document_page_size(IntPtr doc, uint page, out float wPt, out float hPt);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern int prism_document_render_page(
        IntPtr doc,
        uint page,
        uint format,
        float dpi,
        out IntPtr outBuf,
        out UIntPtr outLen);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr prism_document_page_text(IntPtr doc, uint page);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr prism_document_search(IntPtr doc, byte[] query, uint flags);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern int prism_document_export(
        IntPtr doc,
        uint format,
        out IntPtr outBuf,
        out UIntPtr outLen);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr prism_document_font_report(IntPtr doc);

    /// <summary>Library version, e.g. "0.5.0+abc1234".</summary>
    public static string Version => PtrToString(prism_version()) ?? string.Empty;

    /// <summary>Reason for the last failed call on the calling thread, if any.</summary>
    public static string? LastError() => PtrToString(prism_last_error());

    /// <summary>Detected format and confidence, as JSON.</summary>
    public static string DetectFormatJson(byte[] data, string? fileName = null) =>
        Call(prism_detect_format_ex(data, Len(data), Utf8Z(fileName)));

    /// <summary>Self-contained HTML page that loads nothing from the network.</summary>
    public static string ConvertToHtml(byte[] data, string? fileName = null) =>
        Call(prism_convert_to_html_ex(data, Len(data), Utf8Z(fileName)));

    /// <summary>Plain text of the document, for indexing and search.</summary>
    public static string ExtractText(byte[] data, string? fileName = null) =>
        Call(prism_extract_text(data, Len(data), Utf8Z(fileName)));

    /// <summary>Format, page count, attachments and document properties, as JSON.</summary>
    public static string ExtractMetadataJson(byte[] data, string? fileName = null) =>
        Call(prism_extract_metadata(data, Len(data), Utf8Z(fileName)));

    // Helper to marshal string result and free it
    public static string? PtrToStringAndFree(IntPtr ptr)
    {
        if (ptr == IntPtr.Zero) return null;
        try
        {
            return PtrToString(ptr);
        }
        finally
        {
            prism_free_string(ptr);
        }
    }

    internal static byte[] CopyBufferAndFree(IntPtr buf, UIntPtr len)
    {
        int size = checked((int)len.ToUInt64());
        if (buf == IntPtr.Zero || size <= 0)
        {
            if (buf != IntPtr.Zero) prism_buffer_free(buf, len);
            return Array.Empty<byte>();
        }
        try
        {
            var managed = new byte[size];
            Marshal.Copy(buf, managed, 0, size);
            return managed;
        }
        finally
        {
            prism_buffer_free(buf, len);
        }
    }

    internal static string Call(IntPtr ptr) =>
        PtrToStringAndFree(ptr) ?? throw new PrismException(LastError() ?? "Prism call failed");

    internal static UIntPtr Len(byte[] data) => (UIntPtr)data.Length;

    /// <summary>NUL-terminated UTF-8 bytes, or null.</summary>
    internal static byte[]? Utf8Z(string? value)
    {
        if (value == null) return null;
        var bytes = new byte[Encoding.UTF8.GetByteCount(value) + 1];
        Encoding.UTF8.GetBytes(value, 0, value.Length, bytes, 0);
        return bytes;
    }

    private static string? PtrToString(IntPtr ptr)
    {
        if (ptr == IntPtr.Zero) return null;
#if NETSTANDARD2_0
        int len = 0;
        while (Marshal.ReadByte(ptr, len) != 0) len++;
        if (len == 0) return string.Empty;
        byte[] buffer = new byte[len];
        Marshal.Copy(ptr, buffer, 0, len);
        return Encoding.UTF8.GetString(buffer);
#else
        return Marshal.PtrToStringUTF8(ptr);
#endif
    }
}

/// <summary>
/// Managed wrapper around the native <c>PrismDocument</c> page-on-demand handle.
/// </summary>
public sealed class PrismDocument : IDisposable
{
    private IntPtr handle;

    private PrismDocument(IntPtr handle)
    {
        this.handle = handle;
    }

    /// <summary>Open and lay out a document from an in-memory buffer.</summary>
    public static PrismDocument Open(byte[] data, string? fileName = null)
    {
        if (data == null) throw new ArgumentNullException(nameof(data));
        IntPtr ptr = PrismLib.prism_document_open(data, PrismLib.Len(data), PrismLib.Utf8Z(fileName));
        if (ptr == IntPtr.Zero)
        {
            throw new PrismException(PrismLib.LastError() ?? "Failed to open PrismDocument");
        }
        return new PrismDocument(ptr);
    }

    /// <summary>Total number of laid-out pages.</summary>
    public uint PageCount
    {
        get
        {
            EnsureOpen();
            return PrismLib.prism_document_page_count(handle);
        }
    }

    /// <summary>Return the width and height of a 0-based page in points (1/72 inch).</summary>
    public (float WidthPt, float HeightPt) GetPageSize(uint pageIndex)
    {
        EnsureOpen();
        int rc = PrismLib.prism_document_page_size(handle, pageIndex, out float w, out float h);
        if (rc != 0)
        {
            throw new PrismException(PrismLib.LastError() ?? $"Failed to get size for page {pageIndex}");
        }
        return (w, h);
    }

    /// <summary>Render a single 0-based page to SVG, PNG, or an HTML viewer fragment.</summary>
    public byte[] RenderPage(uint pageIndex, PrismPageFormat format, float dpi = 150.0f)
    {
        EnsureOpen();
        int rc = PrismLib.prism_document_render_page(
            handle, pageIndex, (uint)format, dpi, out IntPtr buf, out UIntPtr len);
        if (rc != 0)
        {
            throw new PrismException(PrismLib.LastError() ?? $"Failed to render page {pageIndex}");
        }
        return PrismLib.CopyBufferAndFree(buf, len);
    }

    /// <summary>Render a single 0-based page as a standalone SVG string.</summary>
    public string RenderPageSvg(uint pageIndex) =>
        Encoding.UTF8.GetString(RenderPage(pageIndex, PrismPageFormat.Svg));

    /// <summary>Render a single 0-based page as an HTML viewer section fragment (SVG + text layer).</summary>
    public string RenderPageViewerFragment(uint pageIndex) =>
        Encoding.UTF8.GetString(RenderPage(pageIndex, PrismPageFormat.ViewerFragment));

    /// <summary>Render a single 0-based page as PNG image bytes at the specified DPI.</summary>
    public byte[] RenderPagePng(uint pageIndex, float dpi = 150.0f) =>
        RenderPage(pageIndex, PrismPageFormat.Png, dpi);

    /// <summary>Extract reading-order plain text for a single 0-based page.</summary>
    public string GetPageText(uint pageIndex)
    {
        EnsureOpen();
        return PrismLib.Call(PrismLib.prism_document_page_text(handle, pageIndex));
    }

    /// <summary>Search across all pages and return matching hits and bounding boxes as JSON.</summary>
    public string SearchJson(string query, PrismSearchFlags flags = PrismSearchFlags.None)
    {
        if (query == null) throw new ArgumentNullException(nameof(query));
        EnsureOpen();
        byte[] queryBytes = PrismLib.Utf8Z(query)!;
        return PrismLib.Call(PrismLib.prism_document_search(handle, queryBytes, (uint)flags));
    }

    /// <summary>Export the entire document to PDF, multi-page TIFF, or self-contained HTML viewer.</summary>
    public byte[] Export(PrismExportFormat format)
    {
        EnsureOpen();
        int rc = PrismLib.prism_document_export(handle, (uint)format, out IntPtr buf, out UIntPtr len);
        if (rc != 0)
        {
            throw new PrismException(PrismLib.LastError() ?? $"Failed to export document as {format}");
        }
        return PrismLib.CopyBufferAndFree(buf, len);
    }

    /// <summary>Return the per-document font resolution and substitution report as JSON.</summary>
    public string GetFontReportJson()
    {
        EnsureOpen();
        return PrismLib.Call(PrismLib.prism_document_font_report(handle));
    }

    private void EnsureOpen()
    {
        if (handle == IntPtr.Zero)
        {
            throw new ObjectDisposedException(nameof(PrismDocument));
        }
    }

    public void Dispose()
    {
        IntPtr h = handle;
        if (h != IntPtr.Zero)
        {
            handle = IntPtr.Zero;
            PrismLib.prism_document_close(h);
        }
        GC.SuppressFinalize(this);
    }

    ~PrismDocument()
    {
        Dispose();
    }
}
