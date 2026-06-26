use serde::Serialize;
use std::fmt;

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use std::arch::x86_64::*;

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
pub struct FhirHumanName<'bump, 'a> {
    pub family: Option<ZeroCopyField<'a, &'a str>>,
    pub given: &'bump [ZeroCopyField<'a, &'a str>],
    pub metadata: FieldMetadata,
}

#[derive(Debug, Clone, Serialize)]
pub struct FhirPatient<'bump, 'a> {
    pub resource_type: ZeroCopyField<'a, &'a str>,
    pub id: ZeroCopyField<'a, &'a str>,
    pub active: Option<ZeroCopyField<'a, bool>>,
    pub gender: Option<ZeroCopyField<'a, &'a str>>,
    pub birth_date: Option<ZeroCopyField<'a, &'a str>>,
    pub names: &'bump [FhirHumanName<'bump, 'a>],
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ParseError {
    pub code: u32,
    pub message: &'static str,
    pub offset: usize,
}

impl ParseError {
    pub fn new(code: u32, message: &'static str, offset: usize) -> Self {
        Self {
            code,
            message,
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

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn skip_whitespace_avx2(input: &[u8], mut cursor: usize) -> usize {
    let len = input.len();
    while cursor + 32 <= len {
        // SAFETY: Pointer offset check guarantees bounds within 32-byte chunk.
        let chunk = unsafe { _mm256_loadu_si256(input.as_ptr().add(cursor) as *const __m256i) };
        let space = _mm256_set1_epi8(0x20);
        let tab = _mm256_set1_epi8(0x09);
        let lf = _mm256_set1_epi8(0x0A);
        let cr = _mm256_set1_epi8(0x0D);
        
        let eq_space = _mm256_cmpeq_epi8(chunk, space);
        let eq_tab = _mm256_cmpeq_epi8(chunk, tab);
        let eq_lf = _mm256_cmpeq_epi8(chunk, lf);
        let eq_cr = _mm256_cmpeq_epi8(chunk, cr);
        
        let is_ws = _mm256_or_si256(
            _mm256_or_si256(eq_space, eq_tab),
            _mm256_or_si256(eq_lf, eq_cr)
        );
        
        let mask = _mm256_movemask_epi8(is_ws);
        if mask != -1 {
            let first_non_ws = (!mask).trailing_zeros() as usize;
            return cursor + first_non_ws;
        }
        cursor += 32;
    }
    while cursor < len {
        let b = input[cursor];
        if b == 0x20 || b == 0x09 || b == 0x0A || b == 0x0D {
            cursor += 1;
        } else {
            break;
        }
    }
    cursor
}

#[inline(always)]
fn skip_whitespace_fallback(input: &[u8], mut cursor: usize) -> usize {
    let len = input.len();
    while cursor < len {
        let b = input[cursor];
        if b == 0x20 || b == 0x09 || b == 0x0A || b == 0x0D {
            cursor += 1;
        } else {
            break;
        }
    }
    cursor
}

#[inline(always)]
fn skip_whitespace_simd(input: &[u8], cursor: usize) -> usize {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if is_x86_feature_detected!("avx2") {
            // SAFETY: Checked CPU runtime compatibility before dispatching to AVX2 function.
            return unsafe { skip_whitespace_avx2(input, cursor) };
        }
    }
    skip_whitespace_fallback(input, cursor)
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn scan_string_avx2(input: &[u8], mut cursor: usize) -> (usize, bool) {
    let len = input.len();
    while cursor + 32 <= len {
        // SAFETY: Pointer offset check guarantees bounds within 32-byte chunk.
        let chunk = unsafe { _mm256_loadu_si256(input.as_ptr().add(cursor) as *const __m256i) };
        let quote = _mm256_set1_epi8(b'"' as i8);
        let backslash = _mm256_set1_epi8(b'\\' as i8);
        
        let eq_quote = _mm256_cmpeq_epi8(chunk, quote);
        let eq_bs = _mm256_cmpeq_epi8(chunk, backslash);
        
        let matched = _mm256_or_si256(eq_quote, eq_bs);
        let mask = _mm256_movemask_epi8(matched);
        
        if mask != 0 {
            let first_match = mask.trailing_zeros() as usize;
            let idx = cursor + first_match;
            let is_backslash = input[idx] == b'\\';
            return (idx, is_backslash);
        }
        cursor += 32;
    }
    while cursor < len {
        let b = input[cursor];
        if b == b'"' {
            return (cursor, false);
        } else if b == b'\\' {
            return (cursor, true);
        }
        cursor += 1;
    }
    (len, false)
}

#[inline(always)]
fn scan_string_fallback(input: &[u8], mut cursor: usize) -> (usize, bool) {
    let len = input.len();
    while cursor < len {
        let b = input[cursor];
        if b == b'"' {
            return (cursor, false);
        } else if b == b'\\' {
            return (cursor, true);
        }
        cursor += 1;
    }
    (len, false)
}

#[inline(always)]
fn scan_string_simd(input: &[u8], cursor: usize) -> (usize, bool) {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if is_x86_feature_detected!("avx2") {
            // SAFETY: Checked CPU runtime compatibility before dispatching to AVX2 function.
            return unsafe { scan_string_avx2(input, cursor) };
        }
    }
    scan_string_fallback(input, cursor)
}

pub struct Lexer<'a> {
    input: &'a [u8],
    cursor: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            cursor: 0,
        }
    }

    pub fn next_token(&mut self) -> Result<Option<Token<'a>>, ParseError> {
        self.cursor = skip_whitespace_simd(self.input, self.cursor);
        if self.cursor >= self.input.len() {
            return Ok(None);
        }

        let idx = self.cursor;
        let c = self.input[idx];
        self.cursor += 1;

        match c {
            b'{' => Ok(Some(Token::BraceOpen(idx))),
            b'}' => Ok(Some(Token::BraceClose(idx))),
            b'[' => Ok(Some(Token::BracketOpen(idx))),
            b']' => Ok(Some(Token::BracketClose(idx))),
            b':' => Ok(Some(Token::Colon(idx))),
            b',' => Ok(Some(Token::Comma(idx))),
            b'"' => {
                let start_idx = self.cursor;
                let mut escaped = false;
                loop {
                    let (next_idx, is_backslash) = scan_string_simd(self.input, self.cursor);
                    if next_idx >= self.input.len() {
                        return Err(ParseError::new(401, "Unterminated string", idx));
                    }
                    if is_backslash {
                        escaped = !escaped;
                        self.cursor = next_idx + 1;
                    } else {
                        if escaped {
                            let mut bs_count = 0;
                            let mut temp = next_idx - 1;
                            while temp >= start_idx && self.input[temp] == b'\\' {
                                bs_count += 1;
                                if temp == 0 {
                                    break;
                                }
                                temp -= 1;
                            }
                            if bs_count % 2 == 1 {
                                escaped = false;
                                self.cursor = next_idx + 1;
                                continue;
                            }
                        }
                        
                        self.cursor = next_idx + 1;
                        // SAFETY: Validated token slice bounds directly from verified JSON structure.
                        let slice = unsafe {
                            std::str::from_utf8_unchecked(&self.input[start_idx..next_idx])
                        };
                        return Ok(Some(Token::String(slice, start_idx - 1)));
                    }
                }
            }
            b't' | b'f' => {
                let start_idx = idx;
                let is_true = c == b't';
                let expected: &[u8] = if is_true { b"true" } else { b"false" };
                
                if start_idx + expected.len() <= self.input.len() && &self.input[start_idx..start_idx + expected.len()] == expected {
                    self.cursor = start_idx + expected.len();
                    // SAFETY: Validated token slice bounds directly from verified JSON structure.
                    let slice = unsafe {
                        std::str::from_utf8_unchecked(&self.input[start_idx..self.cursor])
                    };
                    Ok(Some(Token::Bool(is_true, slice, start_idx)))
                } else {
                    Err(ParseError::new(401, "Invalid boolean token", start_idx))
                }
            }
            b'n' => {
                let start_idx = idx;
                if start_idx + 4 <= self.input.len() && &self.input[start_idx..start_idx + 4] == b"null" {
                    self.cursor = start_idx + 4;
                    // SAFETY: Validated token slice bounds directly from verified JSON structure.
                    let slice = unsafe {
                        std::str::from_utf8_unchecked(&self.input[start_idx..self.cursor])
                    };
                    Ok(Some(Token::Null(slice, start_idx)))
                } else {
                    Err(ParseError::new(401, "Invalid null token", start_idx))
                }
            }
            b'-' | b'0'..=b'9' => {
                let start_idx = idx;
                while self.cursor < self.input.len() {
                    let next_c = self.input[self.cursor];
                    if next_c.is_ascii_digit() || next_c == b'.' || next_c == b'e' || next_c == b'E' || next_c == b'+' || next_c == b'-' {
                        self.cursor += 1;
                    } else {
                        break;
                    }
                }
                // SAFETY: Validated token slice bounds directly from verified JSON structure.
                let slice = unsafe {
                    std::str::from_utf8_unchecked(&self.input[start_idx..self.cursor])
                };
                Ok(Some(Token::Number(slice, start_idx)))
            }
            _ => Err(ParseError::new(401, "Unexpected character", idx)),
        }
    }
}

pub struct FhirParser<'bump, 'a> {
    lexer: Lexer<'a>,
    errors: bumpalo::collections::Vec<'bump, ParseError>,
    corrupt_bytes: usize,
    bump: &'bump bumpalo::Bump,
}

impl<'bump, 'a> FhirParser<'bump, 'a> {
    pub fn new(input: &'a str, bump: &'bump bumpalo::Bump) -> Self {
        Self {
            lexer: Lexer::new(input),
            errors: bumpalo::collections::Vec::new_in(bump),
            corrupt_bytes: 0,
            bump,
        }
    }

    pub fn get_errors(&self) -> &[ParseError] {
        &self.errors
    }

    pub fn get_corrupt_bytes(&self) -> usize {
        self.corrupt_bytes
    }

    pub fn parse_patient(&mut self) -> Result<FhirPatient<'bump, 'a>, ParseError> {
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
        let mut names: &'bump [FhirHumanName<'bump, 'a>] = &[];

        loop {
            self.lexer.cursor = skip_whitespace_simd(self.lexer.input, self.lexer.cursor);
            let tok = match self.lexer.next_token() {
                Ok(t) => t,
                Err(err) => {
                    self.errors.push(err);
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
                    self.errors.push(err);
                    self.raw_skip_to_next_field(err.offset);
                    continue;
                }
            };

            let colon_tok = match self.lexer.next_token() {
                Ok(t) => t,
                Err(err) => {
                    self.errors.push(err);
                    self.raw_skip_to_next_field(err.offset);
                    continue;
                }
            };

            match colon_tok {
                Some(Token::Colon(_)) => {}
                _ => {
                    let offset = colon_tok.map_or(self.lexer.input.len(), |t| t.offset());
                    let err = ParseError::new(401, "Expected ':' after key", offset);
                    self.errors.push(err);
                    self.raw_skip_to_next_field(err.offset);
                    continue;
                }
            }

            let value_start_offset = self.lexer.cursor;

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
                    self.errors.push(err);
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
                    self.errors.push(err);
                    self.raw_skip_to_next_field(err.offset);
                }
            }
        }

        let resource_type = match resource_type {
            Some(r) => r,
            None => {
                let err = ParseError::new(401, "Missing required field: resourceType", self.lexer.input.len());
                self.errors.push(err);
                return Err(err);
            }
        };

        let id = match id {
            Some(i) => i,
            None => {
                let err = ParseError::new(401, "Missing required field: id", self.lexer.input.len());
                self.errors.push(err);
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
                // SAFETY: Pointer arithmetic relies on slice offset mapping inside self.lexer.input.
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
                // SAFETY: Pointer arithmetic relies on slice offset mapping inside self.lexer.input.
                let address = s.as_ptr() as usize;
                let offset = s.as_ptr() as usize - self.lexer.input.as_ptr() as usize;
                Ok(ZeroCopyField::new(b, address, offset))
            }
            Some(t) => Err(ParseError::new(401, "Expected boolean value", t.offset())),
            None => Err(ParseError::new(401, "Unexpected end of input", self.lexer.input.len())),
        }
    }

    fn parse_names_array(&mut self) -> Result<&'bump [FhirHumanName<'bump, 'a>], ParseError> {
        let start_tok = self.lexer.next_token()?;
        match start_tok {
            Some(Token::BracketOpen(_)) => {}
            _ => {
                let offset = start_tok.map_or(self.lexer.input.len(), |t| t.offset());
                return Err(ParseError::new(401, "Expected array start '[' for name field", offset));
            }
        }

        let mut names = bumpalo::collections::Vec::new_in(self.bump);
        loop {
            self.lexer.cursor = skip_whitespace_simd(self.lexer.input, self.lexer.cursor);
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

        Ok(names.into_bump_slice())
    }

    fn parse_name_object(&mut self, start_offset: usize) -> Result<FhirHumanName<'bump, 'a>, ParseError> {
        let mut family: Option<ZeroCopyField<'a, &'a str>> = None;
        let mut given = bumpalo::collections::Vec::new_in(self.bump);

        loop {
            self.lexer.cursor = skip_whitespace_simd(self.lexer.input, self.lexer.cursor);
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
                        self.lexer.cursor = skip_whitespace_simd(self.lexer.input, self.lexer.cursor);
                        let next_element = self.lexer.next_token()?;
                        match next_element {
                            Some(Token::BracketClose(_)) => break,
                            Some(Token::String(s, _)) => {
                                // SAFETY: Pointer arithmetic relies on slice offset mapping inside self.lexer.input.
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

        // SAFETY: Pointer offset mapping inside parent JSON memory segment.
        let raw_addr = unsafe { self.lexer.input.as_ptr().add(start_offset) as usize };
        Ok(FhirHumanName {
            family,
            given: given.into_bump_slice(),
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
        if self.lexer.cursor >= self.lexer.input.len() {
            return;
        }
        let remaining = &self.lexer.input[self.lexer.cursor..];
        let mut brace_depth = 0;
        let mut bracket_depth = 0;
        let mut skipped_len = 0;

        for (i, &b) in remaining.iter().enumerate() {
            skipped_len = i + 1;
            match b {
                b'{' => brace_depth += 1,
                b'[' => bracket_depth += 1,
                b'}' => {
                    if brace_depth == 0 {
                        break;
                    }
                    brace_depth -= 1;
                }
                b']' => {
                    if bracket_depth > 0 {
                        bracket_depth -= 1;
                    }
                }
                b',' => {
                    if brace_depth == 0 && bracket_depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }

        self.lexer.cursor += skipped_len;
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

        let bump = bumpalo::Bump::new();
        let mut parser = FhirParser::new(raw, &bump);
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

        let bump = bumpalo::Bump::new();
        let mut parser = FhirParser::new(raw, &bump);
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

        let bump = bumpalo::Bump::new();
        let mut parser = FhirParser::new(raw, &bump);
        let patient = parser.parse_patient().unwrap();

        assert_eq!(patient.resource_type.value, "Patient");
        assert_eq!(patient.id.value, "123");
        assert!(patient.active.is_none());
        assert_eq!(patient.gender.unwrap().value, "other");

        assert!(parser.get_errors().len() > 0);
        assert!(parser.get_corrupt_bytes() > 0);
    }
}
