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
    -   Copy `PrismNative.cs` (from `../dotnet/`) to your project.
    -   Copy `PrismViewer.cs` to your project.

3.  **Usage**:
    -   Build the project.
    -   Open your Main Form designer.
    -   Drag `PrismViewer` from the Toolbox onto your form.
    -   In code, call:
        ```csharp
        prismViewer1.LoadFile("path/to/document.pdf");
        ```

## Architecture

`PrismViewer` matches the following flow:
1.  Reads file bytes.
2.  Calls `PrismNative.prism_convert_to_html`.
3.  Receives HTML string from Rust (Prism SDK).
4.  Renders HTML using `WebView2`.

If conversion fails or format is unsupported, it falls back to a text/hex preview.
