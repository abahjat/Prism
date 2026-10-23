# SPDX-License-Identifier: AGPL-3.0-only
<#
.SYNOPSIS
    Generates reference PDF renders from the Prism test corpus using Microsoft Office COM automation.
.DESCRIPTION
    Opens DOCX/DOC files in Word and PPTX/PPT files in PowerPoint with strict forensic safety settings:
    - ReadOnly = $true
    - AutomationSecurity = 3 (msoAutomationSecurityForceDisable — macros disabled)
    - UpdateFieldsAtPrint = $false, UpdateLinksAtOpen = $false, UpdateLinksAtPrint = $false
    - AddToRecentFiles = $false
    - DisplayAlerts = 0 (no modal dialogs)
    Exports each document as a fixed-layout PDF in `test-files/reference/<name>.pdf`.
#>
[CmdletBinding()]
param(
    [string]$CorpusDir = (Join-Path $PSScriptRoot "..\test-files"),
    [string]$OutputDir = (Join-Path $PSScriptRoot "..\test-files\reference")
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$CorpusDir = (Resolve-Path $CorpusDir).Path
if (-not (Test-Path $OutputDir)) {
    New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
}
$OutputDir = (Resolve-Path $OutputDir).Path

$msoAutomationSecurityForceDisable = 3
$wdExportFormatPDF = 17
$ppSaveAsPDF = 32

Write-Host "Reference output directory: $OutputDir"

# 1. Word documents (.docx, .doc, .rtf)
$wordFiles = Get-ChildItem -Path $CorpusDir -File | Where-Object {
    $_.Extension -in @(".docx", ".doc", ".rtf")
}

if ($wordFiles.Count -gt 0) {
    $word = $null
    try {
        $word = New-Object -ComObject Word.Application
        $word.Visible = $false
        $word.DisplayAlerts = 0 # wdAlertsNone
        $word.AutomationSecurity = $msoAutomationSecurityForceDisable
        $word.Options.UpdateFieldsAtPrint = $false
        $word.Options.UpdateLinksAtOpen = $false
        $word.Options.UpdateLinksAtPrint = $false

        foreach ($file in $wordFiles) {
            $outPdf = Join-Path $OutputDir ($file.BaseName + ".word.pdf")
            Write-Host "[Word] Exporting $($file.Name) -> $outPdf"
            $doc = $null
            try {
                # Open(FileName, ConfirmConversions, ReadOnly, AddToRecentFiles)
                $doc = $word.Documents.Open($file.FullName, $false, $true, $false)
                $doc.ExportAsFixedFormat($outPdf, $wdExportFormatPDF)
            }
            catch {
                Write-Warning "Failed to export $($file.Name): $_"
            }
            finally {
                if ($null -ne $doc) {
                    $doc.Close(0) # wdDoNotSaveChanges
                    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($doc)
                }
            }
        }
    }
    catch {
        Write-Warning "Microsoft Word COM automation unavailable: $_"
    }
    finally {
        if ($null -ne $word) {
            $word.Quit()
            [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($word)
        }
    }
}

# 2. PowerPoint presentations (.pptx, .ppt)
$pptFiles = Get-ChildItem -Path $CorpusDir -File | Where-Object {
    $_.Extension -in @(".pptx", ".ppt")
}

if ($pptFiles.Count -gt 0) {
    $ppt = $null
    try {
        $ppt = New-Object -ComObject PowerPoint.Application
        $ppt.DisplayAlerts = 1 # ppAlertsNone
        $ppt.AutomationSecurity = $msoAutomationSecurityForceDisable

        foreach ($file in $pptFiles) {
            $outPdf = Join-Path $OutputDir ($file.BaseName + ".pptx.pdf")
            Write-Host "[PowerPoint] Exporting $($file.Name) -> $outPdf"
            $pres = $null
            try {
                # Open(FileName, ReadOnly=msoTrue(-1), Untitled=msoFalse(0), WithWindow=msoFalse(0))
                $pres = $ppt.Presentations.Open($file.FullName, -1, 0, 0)
                $pres.SaveAs($outPdf, $ppSaveAsPDF)
            }
            catch {
                Write-Warning "Failed to export $($file.Name): $_"
            }
            finally {
                if ($null -ne $pres) {
                    $pres.Close()
                    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($pres)
                }
            }
        }
    }
    catch {
        Write-Warning "Microsoft PowerPoint COM automation unavailable: $_"
    }
    finally {
        if ($null -ne $ppt) {
            $ppt.Quit()
            [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($ppt)
        }
    }
}

Write-Host "Reference generation complete."
