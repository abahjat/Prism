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

    private static string Call(IntPtr ptr) =>
        PtrToStringAndFree(ptr) ?? throw new PrismException(LastError() ?? "Prism call failed");

    private static UIntPtr Len(byte[] data) => (UIntPtr)data.Length;

    /// <summary>NUL-terminated UTF-8 bytes, or null.</summary>
    private static byte[]? Utf8Z(string? value)
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
