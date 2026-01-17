using System;
using System.Runtime.InteropServices;

namespace Prism.Native;

public static class PrismLib
{
    private const string DllName = "prism_bindings";

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern void prism_init();

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr prism_detect_format(byte[] data, UIntPtr len);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr prism_preview_file(byte[] data, UIntPtr len);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr prism_convert_to_html(byte[] data, UIntPtr len);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern void prism_free_string(IntPtr ptr);

    // Helper to marshal string result and free it
    public static string? PtrToStringAndFree(IntPtr ptr)
    {
        if (ptr == IntPtr.Zero) return null;
        try
        {
#if NETSTANDARD2_0
            int len = 0;
            while (Marshal.ReadByte(ptr, len) != 0) len++;
            if (len == 0) return string.Empty;
            byte[] buffer = new byte[len];
            Marshal.Copy(ptr, buffer, 0, len);
            return System.Text.Encoding.UTF8.GetString(buffer);
#else
            return Marshal.PtrToStringUTF8(ptr);
#endif
        }
        finally
        {
            prism_free_string(ptr);
        }
    }
}
