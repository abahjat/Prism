# Prism WinForms Integration

This folder contains the source code for a **PrismViewer** UserControl that can be added to any Windows Forms application.

## Prerequisites

1.  **.NET 6.0+** (or .NET Framework 4.8+)
2.  **WebView2 Runtime** installed on target machine.
3.  **NuGet Packages**:
    -   `Microsoft.Web.WebView2`

## Integration Steps

1.  **Add Rust Bindings**:
    -   Copy `prism_bindings.dll` (from `target/release/`) to your project output directory (e.g., `bin/Debug/net8.0-windows/`).

2.  **Add Code**:
    -   Reference the `Prism.Native` NuGet package (or copy `bindings/dotnet/Prism.Native/PrismNative.cs`).
    -   Copy `PrismViewer.cs` to your project.

3.  **Usage**:
    -   Build the project.
    -   Open your Main Form designer.
    -   Drag `PrismViewer` from the Toolbox onto your form.
    -   In code, call:
        ```csharp
        await prismViewer1.LoadFileAsync("path/to/document.pdf");
        ```

## Architecture

`PrismViewer` matches the following flow:
1.  Reads file bytes.
2.  Calls `PrismLib.ConvertToHtml` (`prism_convert_to_html_ex`), passing the filename as a format hint.
3.  Writes the self-contained HTML page to a private temporary folder.
4.  Displays it through a WebView2 virtual host (`https://prism-viewer.local/`), which has no size limit (unlike `NavigateToString`, which is limited to 2 MB).

The page itself loads nothing from the network and runs no document scripts. The control also cancels any navigation away from the virtual host and blocks new windows. The temporary folder is deleted when the control is disposed.

If conversion fails or the format is unsupported, it falls back to a text/hex preview and shows the error from `prism_last_error`.
