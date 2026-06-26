use serde::Serialize;
use std::fmt;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct FieldMetadata {
    pub address: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ZeroCopyField<'a, T> {
    pub value: T,
    pub metadata: FieldMetadata,
    #[serde(skip)]
    pub _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a, T> ZeroCopyField<'a, T> {
    pub fn new(value: T, address: usize, offset: usize) -> Self {
        Self {
            value,
            metadata: FieldMetadata { address, offset },
            _marker: std::marker::PhantomData,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FhirHumanName<'a> {
    pub family: Option<ZeroCopyField<'a, &'a str>>,
    pub given: Vec<ZeroCopyField<'a, &'a str>>,
    pub metadata: FieldMetadata,
}

#[derive(Debug, Clone, Serialize)]
pub struct FhirPatient<'a> {
    pub resource_type: ZeroCopyField<'a, &'a str>,
    pub id: ZeroCopyField<'a, &'a str>,
    pub active: Option<ZeroCopyField<'a, bool>>,
    pub gender: Option<ZeroCopyField<'a, &'a str>>,
    pub birth_date: Option<ZeroCopyField<'a, &'a str>>,
    pub names: Vec<FhirHumanName<'a>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ParseError {
    pub code: u32,
    pub message: String,
    pub offset: usize,
}

impl ParseError {
    pub fn new(code: u32, message: impl Into<String>, offset: usize) -> Self {
        Self {
            code,
            message: message.into(),
            offset,
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Error {}: {} at offset {}", self.code, self.message, self.offset)
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token<'a> {
    BraceOpen(usize),
    BraceClose(usize),
    BracketOpen(usize),
    BracketClose(usize),
    Colon(usize),
    Comma(usize),
    String(&'a str, usize),
    Number(&'a str, usize),
    Bool(bool, &'a str, usize),
    Null(&'a str, usize),
}

impl<'a> Token<'a> {
    pub fn offset(&self) -> usize {
        match *self {
            Token::BraceOpen(o) => o,
            Token::BraceClose(o) => o,
            Token::BracketOpen(o) => o,
            Token::BracketClose(o) => o,
            Token::Colon(o) => o,
            Token::Comma(o) => o,
            Token::String(_, o) => o,
            Token::Number(_, o) => o,
            Token::Bool(_, _, o) => o,
            Token::Null(_, o) => o,
        }
    }
}

pub struct Lexer<'a> {
    input: &'a str,
    chars: std::str::CharIndices<'a>,
    current_char: Option<(usize, char)>,
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str) -> Self {
        let mut chars = input.char_indices();
        let current_char = chars.next();
        Self { input, chars, current_char }
    }

    fn bump(&mut self) {
        self.current_char = self.chars.next();
    }

    fn skip_whitespace(&mut self) {
        while let Some((_, c)) = self.current_char {
            if c.is_whitespace() {
                self.bump();
            } else {
                break;
            }
        }
    }

    pub fn next_token(&mut self) -> Result<Option<Token<'a>>, ParseError> {
        self.skip_whitespace();
        let (idx, c) = match self.current_char {
            None => return Ok(None),
            Some(x) => x,
        };

        match c {
            '{' => {
                self.bump();
                Ok(Some(Token::BraceOpen(idx)))
            }
            '}' => {
                self.bump();
                Ok(Some(Token::BraceClose(idx)))
            }
            '[' => {
                self.bump();
                Ok(Some(Token::BracketOpen(idx)))
            }
            ']' => {
                self.bump();
                Ok(Some(Token::BracketClose(idx)))
            }
            ':' => {
                self.bump();
                Ok(Some(Token::Colon(idx)))
            }
            ',' => {
                self.bump();
                Ok(Some(Token::Comma(idx)))
            }
            '"' => {
                let start_idx = idx + 1;
                self.bump();
                let mut escaped = false;
                loop {
                    match self.current_char {
                        Some((end_idx, '"')) if !escaped => {
                            self.bump();
                            let slice = &self.input[start_idx..end_idx];
                            return Ok(Some(Token::String(slice, start_idx - 1)));
                        }
                        Some((_, '\\')) => {
                            escaped = !escaped;
                            self.bump();
                        }
                        Some(_) => {
                            escaped = false;
                            self.bump();
                        }
                        None => return Err(ParseError::new(401, "Unterminated string", idx)),
                    }
                }
            }
            't' | 'f' => {
                let start_idx = idx;
                let is_true = c == 't';
                let expected = if is_true { "true" } else { "false" };
                for expected_char in expected.chars() {
                    match self.current_char {
                        Some((_, actual_char)) if actual_char == expected_char => {
                            self.bump();
                        }
                        _ => return Err(ParseError::new(401, "Invalid boolean token", start_idx)),
                    }
                }
                let slice = &self.input[start_idx..self.current_char.map_or(self.input.len(), |(i, _)| i)];
                Ok(Some(Token::Bool(is_true, slice, start_idx)))
            }
            'n' => {
                let start_idx = idx;
                for expected_char in "null".chars() {
                    match self.current_char {
                        Some((_, actual_char)) if actual_char == expected_char => {
                            self.bump();
                        }
                        _ => return Err(ParseError::new(401, "Invalid null token", start_idx)),
                    }
                }
                let slice = &self.input[start_idx..self.current_char.map_or(self.input.len(), |(i, _)| i)];
                Ok(Some(Token::Null(slice, start_idx)))
            }
            '-' | '0'..='9' => {
                let start_idx = idx;
                self.bump();
                while let Some((_, next_c)) = self.current_char {
                    if next_c.is_ascii_digit() || next_c == '.' || next_c == 'e' || next_c == 'E' || next_c == '+' || next_c == '-' {
                        self.bump();
                    } else {
                        break;
                    }
                }
                let end_idx = self.current_char.map_or(self.input.len(), |(i, _)| i);
                let slice = &self.input[start_idx..end_idx];
                Ok(Some(Token::Number(slice, start_idx)))
            }
            _ => Err(ParseError::new(401, "Unexpected character", idx)),
        }
    }
}

pub struct FhirParser<'a> {
    lexer: Lexer<'a>,
    errors: Vec<ParseError>,
    corrupt_bytes: usize,
}

impl<'a> FhirParser<'a> {
    pub fn new(input: &'a str) -> Self {
        Self {
            lexer: Lexer::new(input),
            errors: Vec::new(),
            corrupt_bytes: 0,
        }
    }

    pub fn get_errors(&self) -> &[ParseError] {
        &self.errors
    }

    pub fn get_corrupt_bytes(&self) -> usize {
        self.corrupt_bytes
    }

    pub fn parse_patient(&mut self) -> Result<FhirPatient<'a>, ParseError> {
        let root_token = self.lexer.next_token()?;
        match root_token {
            Some(Token::BraceOpen(_)) => {}
            _ => {
                let offset = root_token.map_or(0, |t| t.offset());
                return Err(ParseError::new(401, "Expected patient object starting with '{'", offset));
            }
        }

        let mut resource_type: Option<ZeroCopyField<'a, &'a str>> = None;
        let mut id: Option<ZeroCopyField<'a, &'a str>> = None;
        let mut active: Option<ZeroCopyField<'a, bool>> = None;
        let mut gender: Option<ZeroCopyField<'a, &'a str>> = None;
        let mut birth_date: Option<ZeroCopyField<'a, &'a str>> = None;
        let mut names: Vec<FhirHumanName<'a>> = Vec::new();

        loop {
            self.lexer.skip_whitespace();
            let tok = match self.lexer.next_token() {
                Ok(t) => t,
                Err(err) => {
                    self.errors.push(err.clone());
                    self.raw_skip_to_next_field(err.offset);
                    continue;
                }
            };

            let key_token = match tok {
                None => return Err(ParseError::new(401, "Unexpected end of input", self.lexer.input.len())),
                Some(Token::BraceClose(_)) => break,
                Some(Token::String(k, o)) => (k, o),
                Some(t) => {
                    let err = ParseError::new(401, "Expected key string inside object", t.offset());
                    self.errors.push(err.clone());
                    self.raw_skip_to_next_field(err.offset);
                    continue;
                }
            };

            let colon_tok = match self.lexer.next_token() {
                Ok(t) => t,
                Err(err) => {
                    self.errors.push(err.clone());
                    self.raw_skip_to_next_field(err.offset);
                    continue;
                }
            };

            match colon_tok {
                Some(Token::Colon(_)) => {}
                _ => {
                    let offset = colon_tok.map_or(self.lexer.input.len(), |t| t.offset());
                    let err = ParseError::new(401, "Expected ':' after key", offset);
                    self.errors.push(err.clone());
                    self.raw_skip_to_next_field(err.offset);
                    continue;
                }
            }

            let value_start_offset = self.lexer.current_char.map_or(self.lexer.input.len(), |(i, _)| i);

            match key_token.0 {
                "resourceType" => {
                    match self.parse_string_field() {
                        Ok(field) => {
                            if field.value != "Patient" {
                                let err = ParseError::new(401, "resourceType must be 'Patient'", field.metadata.offset);
                                self.errors.push(err);
                            }
                            resource_type = Some(field);
                        }
                        Err(err) => {
                            self.errors.push(err);
                            self.raw_skip_to_next_field(value_start_offset);
                        }
                    }
                }
                "id" => {
                    match self.parse_string_field() {
                        Ok(field) => {
                            if field.value.is_empty() {
                                let err = ParseError::new(401, "Patient id cannot be empty", field.metadata.offset);
                                self.errors.push(err);
                            }
                            id = Some(field);
                        }
                        Err(err) => {
                            self.errors.push(err);
                            self.raw_skip_to_next_field(value_start_offset);
                        }
                    }
                }
                "active" => {
                    match self.parse_bool_field() {
                        Ok(field) => active = Some(field),
                        Err(err) => {
                            self.errors.push(err);
                            self.raw_skip_to_next_field(value_start_offset);
                        }
                    }
                }
                "gender" => {
                    match self.parse_string_field() {
                        Ok(field) => gender = Some(field),
                        Err(err) => {
                            self.errors.push(err);
                            self.raw_skip_to_next_field(value_start_offset);
                        }
                    }
                }
                "birthDate" => {
                    match self.parse_string_field() {
                        Ok(field) => {
                            if !validate_iso_date(field.value) {
                                let err = ParseError::new(402, "Invalid ISO-8601 date format for birthDate", field.metadata.offset);
                                self.errors.push(err);
                            } else {
                                birth_date = Some(field);
                            }
                        }
                        Err(err) => {
                            self.errors.push(err);
                            self.raw_skip_to_next_field(value_start_offset);
                        }
                    }
                }
                "name" => {
                    match self.parse_names_array() {
                        Ok(parsed_names) => names = parsed_names,
                        Err(err) => {
                            self.errors.push(err);
                            self.raw_skip_to_next_field(value_start_offset);
                        }
                    }
                }
                _ => {
                    if let Err(err) = self.skip_value() {
                        self.errors.push(err);
                        self.raw_skip_to_next_field(value_start_offset);
                    }
                }
            }

            let next_tok = match self.lexer.next_token() {
                Ok(t) => t,
                Err(err) => {
                    self.errors.push(err.clone());
                    self.raw_skip_to_next_field(err.offset);
                    continue;
                }
            };

            match next_tok {
                Some(Token::Comma(_)) => {}
                Some(Token::BraceClose(_)) => break,
                _ => {
                    let offset = next_tok.map_or(self.lexer.input.len(), |t| t.offset());
                    let err = ParseError::new(401, "Expected ',' or '}' inside object", offset);
                    self.errors.push(err.clone());
                    self.raw_skip_to_next_field(err.offset);
                }
            }
        }

        let resource_type = match resource_type {
            Some(r) => r,
            None => {
                let err = ParseError::new(401, "Missing required field: resourceType", self.lexer.input.len());
                self.errors.push(err.clone());
                return Err(err);
            }
        };

        let id = match id {
            Some(i) => i,
            None => {
                let err = ParseError::new(401, "Missing required field: id", self.lexer.input.len());
                self.errors.push(err.clone());
                return Err(err);
            }
        };

        Ok(FhirPatient {
            resource_type,
            id,
            active,
            gender,
            birth_date,
            names,
        })
    }

    fn parse_string_field(&mut self) -> Result<ZeroCopyField<'a, &'a str>, ParseError> {
        match self.lexer.next_token()? {
            Some(Token::String(s, _)) => {
                // SAFETY: Casting reference to raw pointer to obtain memory address for UI highlight visualization.
                let address = s.as_ptr() as usize;
                let offset = s.as_ptr() as usize - self.lexer.input.as_ptr() as usize;
                Ok(ZeroCopyField::new(s, address, offset))
            }
            Some(t) => Err(ParseError::new(401, "Expected string value", t.offset())),
            None => Err(ParseError::new(401, "Unexpected end of input", self.lexer.input.len())),
        }
    }

    fn parse_bool_field(&mut self) -> Result<ZeroCopyField<'a, bool>, ParseError> {
        match self.lexer.next_token()? {
            Some(Token::Bool(b, s, _)) => {
                // SAFETY: Casting reference to raw pointer to obtain memory address for UI highlight visualization.
                let address = s.as_ptr() as usize;
                let offset = s.as_ptr() as usize - self.lexer.input.as_ptr() as usize;
                Ok(ZeroCopyField::new(b, address, offset))
            }
            Some(t) => Err(ParseError::new(401, "Expected boolean value", t.offset())),
            None => Err(ParseError::new(401, "Unexpected end of input", self.lexer.input.len())),
        }
    }

    fn parse_names_array(&mut self) -> Result<Vec<FhirHumanName<'a>>, ParseError> {
        let start_tok = self.lexer.next_token()?;
        match start_tok {
            Some(Token::BracketOpen(_)) => {}
            _ => {
                let offset = start_tok.map_or(self.lexer.input.len(), |t| t.offset());
                return Err(ParseError::new(401, "Expected array start '[' for name field", offset));
            }
        }

        let mut names = Vec::new();
        loop {
            self.lexer.skip_whitespace();
            let next_tok = self.lexer.next_token()?;
            match next_tok {
                Some(Token::BracketClose(_)) => break,
                Some(Token::BraceOpen(idx)) => {
                    let name_element = self.parse_name_object(idx)?;
                    names.push(name_element);
                }
                _ => {
                    let offset = next_tok.map_or(self.lexer.input.len(), |t| t.offset());
                    return Err(ParseError::new(401, "Expected name object inside array", offset));
                }
            }

            let separator = self.lexer.next_token()?;
            match separator {
                Some(Token::Comma(_)) => {}
                Some(Token::BracketClose(_)) => break,
                _ => {
                    let offset = separator.map_or(self.lexer.input.len(), |t| t.offset());
                    return Err(ParseError::new(401, "Expected ',' or ']' after name object", offset));
                }
            }
        }

        Ok(names)
    }

    fn parse_name_object(&mut self, start_offset: usize) -> Result<FhirHumanName<'a>, ParseError> {
        let mut family: Option<ZeroCopyField<'a, &'a str>> = None;
        let mut given: Vec<ZeroCopyField<'a, &'a str>> = Vec::new();

        loop {
            self.lexer.skip_whitespace();
            let tok = self.lexer.next_token()?;
            let key = match tok {
                Some(Token::String(k, _)) => k,
                Some(Token::BraceClose(_)) => break,
                _ => {
                    let offset = tok.map_or(self.lexer.input.len(), |t| t.offset());
                    return Err(ParseError::new(401, "Expected key inside name object", offset));
                }
            };

            let colon = self.lexer.next_token()?;
            match colon {
                Some(Token::Colon(_)) => {}
                _ => {
                    let offset = colon.map_or(self.lexer.input.len(), |t| t.offset());
                    return Err(ParseError::new(401, "Expected ':' after name key", offset));
                }
            }

            match key {
                "family" => {
                    family = Some(self.parse_string_field()?);
                }
                "given" => {
                    let open_bracket = self.lexer.next_token()?;
                    match open_bracket {
                        Some(Token::BracketOpen(_)) => {}
                        _ => {
                            let offset = open_bracket.map_or(self.lexer.input.len(), |t| t.offset());
                            return Err(ParseError::new(401, "Expected '[' for given array", offset));
                        }
                    }

                    loop {
                        self.lexer.skip_whitespace();
                        let next_element = self.lexer.next_token()?;
                        match next_element {
                            Some(Token::BracketClose(_)) => break,
                            Some(Token::String(s, _)) => {
                                // SAFETY: Casting reference to raw pointer to obtain memory address for UI highlight visualization.
                                let address = s.as_ptr() as usize;
                                let offset = s.as_ptr() as usize - self.lexer.input.as_ptr() as usize;
                                given.push(ZeroCopyField::new(s, address, offset));
                            }
                            _ => {
                                let offset = next_element.map_or(self.lexer.input.len(), |t| t.offset());
                                return Err(ParseError::new(401, "Expected string inside given array", offset));
                            }
                        }

                        let sep = self.lexer.next_token()?;
                        match sep {
                            Some(Token::Comma(_)) => {}
                            Some(Token::BracketClose(_)) => break,
                            _ => {
                                let offset = sep.map_or(self.lexer.input.len(), |t| t.offset());
                                return Err(ParseError::new(401, "Expected ',' or ']' inside given array", offset));
                            }
                        }
                    }
                }
                _ => {
                    self.skip_value()?;
                }
            }

            let next_tok = self.lexer.next_token()?;
            match next_tok {
                Some(Token::Comma(_)) => {}
                Some(Token::BraceClose(_)) => break,
                _ => {
                    let offset = next_tok.map_or(self.lexer.input.len(), |t| t.offset());
                    return Err(ParseError::new(401, "Expected ',' or '}' inside name object", offset));
                }
            }
        }

        // SAFETY: Obtaining the address of the parent struct start byte index in the JSON document block.
        let raw_addr = unsafe { self.lexer.input.as_ptr().add(start_offset) as usize };
        Ok(FhirHumanName {
            family,
            given,
            metadata: FieldMetadata {
                address: raw_addr,
                offset: start_offset,
            },
        })
    }

    fn skip_value(&mut self) -> Result<(), ParseError> {
        let mut brace_depth = 0;
        let mut bracket_depth = 0;
        loop {
            let tok = match self.lexer.next_token()? {
                None => return Ok(()),
                Some(t) => t,
            };
            match tok {
                Token::BraceOpen(_) => brace_depth += 1,
                Token::BracketOpen(_) => bracket_depth += 1,
                Token::BraceClose(_) => {
                    if brace_depth == 0 {
                        return Ok(());
                    }
                    brace_depth -= 1;
                }
                Token::BracketClose(_) => {
                    if bracket_depth == 0 {
                        return Ok(());
                    }
                    bracket_depth -= 1;
                }
                _ => {}
            }
            if brace_depth == 0 && bracket_depth == 0 {
                return Ok(());
            }
        }
    }

    fn raw_skip_to_next_field(&mut self, start_offset: usize) {
        let remaining = &self.lexer.input[self.lexer.current_char.map_or(self.lexer.input.len(), |(i, _)| i)..];
        let mut brace_depth = 0;
        let mut bracket_depth = 0;
        let mut skipped_len = 0;

        for (i, c) in remaining.char_indices() {
            skipped_len = i + c.len_utf8();
            match c {
                '{' => brace_depth += 1,
                '[' => bracket_depth += 1,
                '}' => {
                    if brace_depth == 0 {
                        break;
                    }
                    brace_depth -= 1;
                }
                ']' => {
                    if bracket_depth > 0 {
                        bracket_depth -= 1;
                    }
                }
                ',' => {
                    if brace_depth == 0 && bracket_depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }

        for _ in 0..skipped_len {
            self.lexer.bump();
        }

        let end_offset = start_offset + skipped_len;
        self.corrupt_bytes += end_offset - start_offset;
    }
}

pub fn validate_iso_date(s: &str) -> bool {
    if s.len() != 10 {
        return false;
    }
    let bytes = s.as_bytes();
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let is_digit = |b: u8| b.is_ascii_digit();
    if !is_digit(bytes[0]) || !is_digit(bytes[1]) || !is_digit(bytes[2]) || !is_digit(bytes[3])
        || !is_digit(bytes[5]) || !is_digit(bytes[6])
        || !is_digit(bytes[8]) || !is_digit(bytes[9])
    {
        return false;
    }

    let year = s[0..4].parse::<i32>().unwrap_or(0);
    let month = s[5..7].parse::<u32>().unwrap_or(0);
    let day = s[8..10].parse::<u32>().unwrap_or(0);

    if year < 1800 || year > 2100 {
        return false;
    }
    if month < 1 || month > 12 {
        return false;
    }

    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let is_leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
            if is_leap { 29 } else { 28 }
        }
        _ => return false,
    };

    day >= 1 && day <= days_in_month
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_patient() {
        let raw = r#"{
            "resourceType": "Patient",
            "id": "123",
            "active": true,
            "gender": "male",
            "birthDate": "1990-01-01",
            "name": [
                {
                    "family": "Smith",
                    "given": ["John", "Paul"]
                }
            ]
        }"#;

        let mut parser = FhirParser::new(raw);
        let patient = parser.parse_patient().unwrap();

        assert_eq!(patient.resource_type.value, "Patient");
        assert_eq!(patient.id.value, "123");
        assert_eq!(patient.active.unwrap().value, true);
        assert_eq!(patient.gender.unwrap().value, "male");
        assert_eq!(patient.birth_date.unwrap().value, "1990-01-01");
        assert_eq!(patient.names.len(), 1);
        assert_eq!(patient.names[0].family.as_ref().unwrap().value, "Smith");
        assert_eq!(patient.names[0].given[0].value, "John");
        assert_eq!(patient.names[0].given[1].value, "Paul");

        let base_ptr = raw.as_ptr() as usize;
        assert_eq!(patient.id.metadata.address, base_ptr + patient.id.metadata.offset);
        assert_eq!(parser.get_errors().len(), 0);
        assert_eq!(parser.get_corrupt_bytes(), 0);
    }

    #[test]
    fn test_corrupt_date_recovery() {
        let raw = r#"{
            "resourceType": "Patient",
            "id": "123",
            "birthDate": "1990/01/01",
            "gender": "female"
        }"#;

        let mut parser = FhirParser::new(raw);
        let patient = parser.parse_patient().unwrap();

        assert_eq!(patient.resource_type.value, "Patient");
        assert_eq!(patient.id.value, "123");
        assert!(patient.birth_date.is_none());
        assert_eq!(patient.gender.unwrap().value, "female");

        let errors = parser.get_errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, 402);
    }

    #[test]
    fn test_structural_chaos_recovery() {
        let raw = r#"{
            "resourceType": "Patient",
            "id": "123",
            "active": { "bad_nested": [1, 2, 3] } ,
            "gender": "other"
        }"#;

        let mut parser = FhirParser::new(raw);
        let patient = parser.parse_patient().unwrap();

        assert_eq!(patient.resource_type.value, "Patient");
        assert_eq!(patient.id.value, "123");
        assert!(patient.active.is_none());
        assert_eq!(patient.gender.unwrap().value, "other");

        assert!(parser.get_errors().len() > 0);
        assert!(parser.get_corrupt_bytes() > 0);
    }
}
