use std::borrow::Cow;

pub fn mask_phi_name<'a>(name: &'a str) -> Cow<'a, str> {
    if name.is_empty() {
        return Cow::Borrowed(name);
    }
    
    let mut modified = false;
    let mut result = String::with_capacity(name.len());
    
    let mut words = name.split_whitespace().peekable();
    while let Some(word) = words.next() {
        let mut chars = word.chars();
        if let Some(first_char) = chars.next() {
            result.push(first_char);
            for _ in chars {
                result.push('*');
                modified = true;
            }
        }
        if words.peek().is_some() {
            result.push(' ');
        }
    }
    
    if modified {
        Cow::Owned(result)
    } else {
        Cow::Borrowed(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_phi_name() {
        assert_eq!(mask_phi_name("Max Mustermann"), "M** M*********");
        assert_eq!(mask_phi_name("John"), "J***");
        assert_eq!(mask_phi_name("A"), "A");
        assert_eq!(mask_phi_name(""), "");
    }
}
