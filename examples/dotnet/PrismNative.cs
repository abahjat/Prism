using System.Runtime.InteropServices;

namespace PrismClient;

public static class PrismNative
{
    private const string DllName = "prism_bindings.dll";

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern void prism_init();

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr prism_detect_format(byte[] data, UIntPtr len);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr prism_preview_file(byte[] data, UIntPtr len);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    public static extern void prism_free_string(IntPtr ptr);

    // Helper to marshal string result and free it
    public static string? PtrToStringAndFree(IntPtr ptr)
    {
        if (ptr == IntPtr.Zero) return null;
        try
        {
            return Marshal.PtrToStringUTF8(ptr);
        }
        finally
        {
            prism_free_string(ptr);
        }
    }
}
