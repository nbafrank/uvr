use std::collections::BTreeMap;

/// Parse a DCF (Debian Control File) block into a `BTreeMap<field, value>`.
/// Continuation lines (leading whitespace) are joined with a space.
///
/// This is the canonical DCF parser shared by the CRAN index parser,
/// the Bioconductor index parser, and the DESCRIPTION file parser.
pub fn parse_dcf_fields(content: &str) -> BTreeMap<String, String> {
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    let mut current_key: Option<String> = None;
    let mut current_value = String::new();

    for line in content.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            // Continuation line
            if current_key.is_some() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    if !current_value.is_empty() {
                        current_value.push(' ');
                    }
                    current_value.push_str(trimmed);
                }
            }
        } else if let Some(colon_pos) = line.find(':') {
            // Save previous field
            if let Some(key) = current_key.take() {
                fields.insert(key, current_value.trim().to_string());
                current_value.clear();
            }
            current_key = Some(line[..colon_pos].trim().to_string());
            current_value = line[colon_pos + 1..].trim().to_string();
        }
    }
    // Save last field
    if let Some(key) = current_key {
        fields.insert(key, current_value.trim().to_string());
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_dcf() {
        let input = "Package: ggplot2\nVersion: 3.4.4\nImports: dplyr,\n  rlang\n";
        let fields = parse_dcf_fields(input);
        assert_eq!(fields["Package"], "ggplot2");
        assert_eq!(fields["Version"], "3.4.4");
        assert_eq!(fields["Imports"], "dplyr, rlang");
    }

    #[test]
    fn continuation_with_tabs() {
        let input = "Description: A long\n\tdescription here\n";
        let fields = parse_dcf_fields(input);
        assert_eq!(fields["Description"], "A long description here");
    }

    #[test]
    fn colon_in_value() {
        let input = "URL: https://example.com\n";
        let fields = parse_dcf_fields(input);
        assert_eq!(fields["URL"], "https://example.com");
    }
}
