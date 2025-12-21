use std::io::Read;
use zip::ZipArchive;

fn main() {
    let file_path = "test-files/2019_self_performance_review.docx";
    
    let file = std::fs::File::open(file_path).expect("Failed to open file");
    let mut archive = ZipArchive::new(file).expect("Failed to open ZIP");
    
    // Find and read word/document.xml
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).expect("Failed to read entry");
        if file.name() == "word/document.xml" {
            let mut xml = String::new();
            file.read_to_string(&mut xml).expect("Failed to read XML");
            
            // Count w:p tags
            let p_count = xml.matches("<w:p>").count() + xml.matches("<w:p ").count();
            let t_count = xml.matches("<w:t>").count() + xml.matches("<w:t ").count();
            
            println!("Found {} paragraph tags", p_count);
            println!("Found {} text tags", t_count);
            println!("\nFirst 2000 chars of XML:");
            println!("{}", &xml[..xml.len().min(2000)]);
            break;
        }
    }
}
