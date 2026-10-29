use std::iter::Peekable;
use std::str::Chars;

pub(super) fn decode(source: &str) -> Result<String, String> {
    let quote = source.chars().next().ok_or("empty metadata string")?;
    if !matches!(quote, '\'' | '"') || !source.ends_with(quote) {
        return Err("metadata strings must use single or double quotes".into());
    }
    let mut input = source[1..source.len() - 1].chars().peekable();
    let mut output = String::new();
    while let Some(character) = input.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        let escaped = input.next().ok_or("unfinished metadata escape")?;
        match escaped {
            'n' => output.push('\n'),
            'r' => output.push('\r'),
            't' => output.push('\t'),
            'b' => output.push('\u{8}'),
            'f' => output.push('\u{c}'),
            'v' => output.push('\u{b}'),
            '\n' | '\u{2028}' | '\u{2029}' => {}
            '\r' => {
                if input.peek() == Some(&'\n') {
                    input.next();
                }
            }
            '0' if !input.peek().is_some_and(char::is_ascii_digit) => output.push('\0'),
            '0'..='9' => return Err("octal escapes are unavailable in workflow metadata".into()),
            'x' => {
                output.push(char::from_u32(hex(&mut input, 2)?).ok_or("invalid metadata escape")?)
            }
            'u' => {
                let code = if input.peek() == Some(&'{') {
                    input.next();
                    let mut digits = String::new();
                    loop {
                        match input.next() {
                            Some('}') if !digits.is_empty() => break,
                            Some(character)
                                if character.is_ascii_hexdigit() && digits.len() < 6 =>
                            {
                                digits.push(character)
                            }
                            _ => return Err("invalid Unicode metadata escape".into()),
                        }
                    }
                    u32::from_str_radix(&digits, 16).map_err(|error| error.to_string())?
                } else {
                    hex(&mut input, 4)?
                };
                let code = if (0xd800..=0xdbff).contains(&code) {
                    if input.next() != Some('\\') || input.next() != Some('u') {
                        return Err("metadata strings cannot contain lone surrogates".into());
                    }
                    let low = hex(&mut input, 4)?;
                    if !(0xdc00..=0xdfff).contains(&low) {
                        return Err("invalid metadata surrogate pair".into());
                    }
                    0x10000 + ((code - 0xd800) << 10) + low - 0xdc00
                } else {
                    code
                };
                output.push(
                    char::from_u32(code).ok_or("metadata strings must contain valid Unicode")?,
                );
            }
            character => output.push(character),
        }
    }
    Ok(output)
}

fn hex(input: &mut Peekable<Chars<'_>>, count: usize) -> Result<u32, String> {
    let mut value = 0;
    for _ in 0..count {
        value = value * 16
            + input
                .next()
                .and_then(|character| character.to_digit(16))
                .ok_or("invalid hexadecimal metadata escape")?;
    }
    Ok(value)
}
