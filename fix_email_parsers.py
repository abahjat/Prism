#!/usr/bin/env python3
"""
Fix email parsers to match current TextRun/TextBlock API
"""

import re

def fix_mbox():
    with open('crates/prism-parsers/src/email/mbox.rs', 'r', encoding='utf-8') as f:
        content = f.read()

    # Fix format_email_header method
    content = re.sub(
        r'(fn format_email_header\(&self, label: &str, value: &str\) -> TextRun \{\s+TextRun \{\s+text: format!\("\{}: \{}\}\\n", label, value\),\s+style: TextStyle \{\s+bold: label == "From" \|\| label == "To" \|\| label == "Subject",\s+\.\.Default::default\(\)\s+\},)\s+(\})',
        r'\1\n            bounds: None,\n            char_positions: None,\n        \2',
        content,
        flags=re.DOTALL
    )

    # Fix TextRun creations with newlines
    content = re.sub(
        r'text_runs\.push\(TextRun \{\s+text: "\\n"\.to_string\(\),\s+style: Default::default\(\),\s+\}\);',
        '''text_runs.push(TextRun {
            text: "\\n".to_string(),
            style: Default::default(),
            bounds: None,
            char_positions: None,
        });''',
        content
    )

    # Fix TextRun creation with body text
    content = re.sub(
        r'text_runs\.push\(TextRun \{\s+text: body_text,\s+style: Default::default\(\),\s+\}\);',
        '''text_runs.push(TextRun {
            text: body_text,
            style: Default::default(),
            bounds: None,
            char_positions: None,
        });''',
        content
    )

    # Fix TextBlock creation
    content = re.sub(
        r'let text_block = TextBlock \{\s+runs: text_runs,\s+bounds: None,\s+\};',
        '''let text_block = TextBlock {
                            runs: text_runs,
                            bounds: prism_core::document::Rect {
                                x: 0.0,
                                y: 0.0,
                                width: Dimensions::LETTER.width,
                                height: Dimensions::LETTER.height,
                            },
                            paragraph_style: None,
                        };''',
        content
    )

    # Fix mail-parser address API
    content = content.replace(
        'addr.address.as_ref().unwrap_or(&String::new())',
        'addr.address.as_ref().map(|a| a.to_string()).unwrap_or_default()'
    )
    content = content.replace(
        'addr.address.clone().unwrap_or_default()',
        'addr.address.as_ref().map(|a| a.to_string()).unwrap_or_default()'
    )

    with open('crates/prism-parsers/src/email/mbox.rs', 'w', encoding='utf-8') as f:
        f.write(content)
    print("Fixed mbox.rs")

def fix_msg():
    with open('crates/prism-parsers/src/email/msg.rs', 'r', encoding='utf-8') as f:
        content = f.read()

    # Remove unused warn import
    content = content.replace('use tracing::{debug, info, warn};', 'use tracing::{debug, info};')

    # Fix format_email_header method
    content = re.sub(
        r'(fn format_email_header\(&self, label: &str, value: &str\) -> TextRun \{\s+TextRun \{\s+text: format!\("\{}: \{}\}\\n", label, value\),\s+style: TextStyle \{\s+bold: label == "From" \|\| label == "To" \|\| label == "Subject",\s+\.\.Default::default\(\)\s+\},)\s+(\})',
        r'\1\n            bounds: None,\n            char_positions: None,\n        \2',
        content,
        flags=re.DOTALL
    )

    # Fix TextRun creations
    content = re.sub(
        r'text_runs\.push\(TextRun \{\s+text: "\\n"\.to_string\(\),\s+style: Default::default\(\),\s+\}\);',
        '''text_runs.push(TextRun {
            text: "\\n".to_string(),
            style: Default::default(),
            bounds: None,
            char_positions: None,
        });''',
        content
    )

    content = re.sub(
        r'text_runs\.push\(TextRun \{\s+text: body_text\.clone\(\),\s+style: Default::default\(\),\s+\}\);',
        '''text_runs.push(TextRun {
            text: body_text.clone(),
            style: Default::default(),
            bounds: None,
            char_positions: None,
        });''',
        content
    )

    # Fix TextBlock creation
    content = re.sub(
        r'let text_block = TextBlock \{\s+runs: text_runs,\s+bounds: None,\s+\};',
        '''let text_block = TextBlock {
            runs: text_runs,
            bounds: prism_core::document::Rect {
                x: 0.0,
                y: 0.0,
                width: Dimensions::LETTER.width,
                height: Dimensions::LETTER.height,
            },
            paragraph_style: None,
        };''',
        content
    )

    with open('crates/prism-parsers/src/email/msg.rs', 'w', encoding='utf-8') as f:
        f.write(content)
    print("Fixed msg.rs")

def fix_vcf():
    with open('crates/prism-parsers/src/email/vcf.rs', 'r', encoding='utf-8') as f:
        content = f.read()

    # Fix format_field method
    content = re.sub(
        r'(fn format_field\(&self, label: &str, value: &str, bold: bool\) -> TextRun \{\s+TextRun \{\s+text: format!\("\{}: \{}\}\\n", label, value\),\s+style: TextStyle \{\s+bold,\s+\.\.Default::default\(\)\s+\},)\s+(\})',
        r'\1\n            bounds: None,\n            char_positions: None,\n        \2',
        content,
        flags=re.DOTALL
    )

    # Fix TextRun creation with bold title
    content = re.sub(
        r'text_runs\.push\(TextRun \{\s+text: "\\nNote:\\n"\.to_string\(\),\s+style: TextStyle \{\s+bold: true,\s+\.\.Default::default\(\)\s+\},\s+\}\);',
        '''text_runs.push(TextRun {
                text: "\\nNote:\\n".to_string(),
                style: TextStyle {
                    bold: true,
                    ..Default::default()
                },
                bounds: None,
                char_positions: None,
            });''',
        content
    )

    # Fix TextRun creation with note content
    content = re.sub(
        r'text_runs\.push\(TextRun \{\s+text: format!\("\{}\}\\n", note\),\s+style: Default::default\(\),\s+\}\);',
        '''text_runs.push(TextRun {
                text: format!("{}\\n", note),
                style: Default::default(),
                bounds: None,
                char_positions: None,
            });''',
        content
    )

    # Fix TextBlock creation
    content = re.sub(
        r'let text_block = TextBlock \{\s+runs: text_runs,\s+bounds: None,\s+\};',
        '''let text_block = TextBlock {
                runs: text_runs,
                bounds: prism_core::document::Rect {
                    x: 0.0,
                    y: 0.0,
                    width: Dimensions::LETTER.width,
                    height: Dimensions::LETTER.height,
                },
                paragraph_style: None,
            };''',
        content
    )

    # Fix address filter type annotation
    content = re.sub(
        r'\.filter\(\|s\| !s\.is_empty\(\)\)',
        r'.filter(|s: &&str| !s.is_empty())',
        content
    )

    with open('crates/prism-parsers/src/email/vcf.rs', 'w', encoding='utf-8') as f:
        f.write(content)
    print("Fixed vcf.rs")

if __name__ == '__main__':
    fix_mbox()
    fix_msg()
    fix_vcf()
    print("All email parsers fixed!")
