#!/usr/bin/env python3
"""
Parse TrID XML definitions and extract signatures relevant to Prism.
Focus on document formats, office files, images, and common text formats.
"""

import os
import xml.etree.ElementTree as ET
from pathlib import Path
from collections import defaultdict
import json

# Relevant MIME type prefixes and patterns
RELEVANT_MIME_PATTERNS = [
    'application/pdf',
    'application/msword',
    'application/vnd.ms-',
    'application/vnd.openxmlformats',
    'application/vnd.oasis.opendocument',
    'application/rtf',
    'application/epub',
    'application/xps',
    'application/zip',
    'application/x-zip',
    'application/gzip',
    'application/x-tar',
    'application/x-rar',
    'application/x-7z',
    'application/json',
    'application/xml',
    'application/x-cfb',
    'text/',
    'image/',
    'message/',
    'multipart/',
]

# Relevant file extensions to look for
RELEVANT_EXTENSIONS = [
    'pdf', 'doc', 'docx', 'xls', 'xlsx', 'ppt', 'pptx',
    'odt', 'ods', 'odp', 'odg', 'rtf', 'txt', 'csv',
    'htm', 'html', 'xml', 'json', 'md', 'markdown',
    'epub', 'xps', 'oxps', 'mobi',
    'eml', 'msg', 'mht', 'mhtml', 'vcf', 'ics',
    'png', 'jpg', 'jpeg', 'gif', 'bmp', 'tif', 'tiff', 'webp', 'svg',
    'zip', 'tar', 'gz', 'rar', '7z',
    'one', 'onenote', 'vsd', 'vsdx', 'mpp',
    'wps', 'wri',  # Legacy formats
]

def is_relevant_format(ext, mime):
    """Check if a format is relevant to Prism."""
    ext_lower = ext.lower() if ext else ''
    mime_lower = mime.lower() if mime else ''
    
    # Check extensions
    for e in RELEVANT_EXTENSIONS:
        if e in ext_lower:
            return True
    
    # Check MIME patterns
    for pattern in RELEVANT_MIME_PATTERNS:
        if pattern in mime_lower:
            return True
    
    return False

def parse_trid_xml(filepath):
    """Parse a single TrID XML file and extract signature info."""
    try:
        tree = ET.parse(filepath)
        root = tree.getroot()
        
        info = root.find('Info')
        if info is None:
            return None
        
        file_type = info.findtext('FileType', '')
        ext = info.findtext('Ext', '')
        mime = info.findtext('Mime', '')
        
        # Extract front block patterns (magic bytes)
        patterns = []
        front_block = root.find('FrontBlock')
        if front_block is not None:
            for pattern in front_block.findall('Pattern'):
                bytes_hex = pattern.findtext('Bytes', '')
                pos = pattern.findtext('Pos', '0')
                if bytes_hex:
                    patterns.append({
                        'bytes': bytes_hex,
                        'pos': int(pos) if pos else 0
                    })
        
        if not patterns:
            return None
        
        return {
            'file_type': file_type,
            'ext': ext,
            'mime': mime,
            'patterns': patterns,
            'source_file': os.path.basename(filepath)
        }
    except Exception as e:
        return None

def main():
    defs_dir = Path(r"C:\Downloads\triddefs_xml\defs")
    
    results = []
    total = 0
    relevant = 0
    
    for xml_file in defs_dir.rglob("*.trid.xml"):
        total += 1
        data = parse_trid_xml(xml_file)
        if data and is_relevant_format(data['ext'], data['mime']):
            relevant += 1
            results.append(data)
    
    print(f"Processed: {total} files")
    print(f"Relevant: {relevant} files")
    print()
    
    # Group by first pattern byte for easy lookup
    by_first_bytes = defaultdict(list)
    for r in results:
        first_pattern = r['patterns'][0]['bytes'][:8] if r['patterns'] else ''
        by_first_bytes[first_pattern].append(r)
    
    # Print grouped results
    print("=" * 80)
    print("MAGIC BYTE SIGNATURES")
    print("=" * 80)
    
    for magic, formats in sorted(by_first_bytes.items()):
        print(f"\nMagic: {magic}")
        for f in formats:
            print(f"  - {f['file_type']}")
            print(f"    Ext: {f['ext']}, MIME: {f['mime']}")
            if len(f['patterns']) > 1:
                print(f"    Additional patterns: {len(f['patterns'])}")
    
    # Export to JSON for further processing
    output_file = Path(r"C:\Dev\RustSandbox\Prism\test-files\trid_signatures.json")
    output_file.parent.mkdir(parents=True, exist_ok=True)
    with open(output_file, 'w', encoding='utf-8') as f:
        json.dump(results, f, indent=2)
    print(f"\nExported {len(results)} signatures to {output_file}")

if __name__ == "__main__":
    main()
