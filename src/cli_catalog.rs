use std::io::{self, BufRead, IsTerminal, Write};

use utterpipe_pocket_tts::voice::CuratedVoice;

const PAGE_ITEMS: usize = 8;

pub(crate) fn is_interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal() && io::stderr().is_terminal()
}

pub(crate) fn print_available(
    voices: &[CuratedVoice],
    installed: &[bool],
    json: bool,
) -> Result<(), String> {
    if installed.len() != voices.len() {
        return Err("curated catalog state is inconsistent".to_owned());
    }
    if json {
        let mut items = Vec::with_capacity(voices.len());
        for (index, (voice, installed)) in voices.iter().zip(installed).enumerate() {
            let mut descriptor = serde_json::to_value(voice)
                .map_err(|_| "could not encode curated voice descriptor".to_owned())?;
            descriptor["number"] = serde_json::Value::from(index + 1);
            descriptor["status"] = serde_json::Value::String(
                if *installed { "installed" } else { "available" }.to_owned(),
            );
            items.push(descriptor);
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": 1,
                "voices": items
            }))
            .map_err(|_| "could not encode curated voice catalog".to_owned())?
        );
        return Ok(());
    }

    if !is_interactive() {
        let stdout = io::stdout();
        render_catalog(&mut stdout.lock(), voices, installed)?;
        return Ok(());
    }

    let stdin = io::stdin();
    let stdout = io::stdout();
    let stderr = io::stderr();
    show_pages(
        &mut stdin.lock(),
        &mut stdout.lock(),
        &mut stderr.lock(),
        voices,
        installed,
    )?;
    Ok(())
}

pub(crate) fn choose_interactively<'a>(
    voices: &'a [CuratedVoice],
    installed: &[bool],
) -> Result<Vec<&'a CuratedVoice>, String> {
    if !is_interactive() {
        return Err(
            "voice installation requires one or more catalog numbers when non-interactive"
                .to_owned(),
        );
    }
    let stdin = io::stdin();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    let mut prompts = stderr.lock();
    if !show_pages(&mut input, &mut output, &mut prompts, voices, installed)? {
        return Err("voice installation cancelled".to_owned());
    }
    prompts
        .write_all(b"Select voices by number (commas/spaces and ranges such as 2-4; q to cancel): ")
        .and_then(|()| prompts.flush())
        .map_err(|_| "could not write voice selection prompt".to_owned())?;
    let mut selection = String::new();
    input
        .read_line(&mut selection)
        .map_err(|_| "could not read voice selection".to_owned())?;
    let selection = selection.trim();
    if selection.is_empty() || selection.eq_ignore_ascii_case("q") {
        return Err("voice installation cancelled".to_owned());
    }
    resolve_selections(&[selection.to_owned()], voices)
}

pub(crate) fn resolve_selections<'a>(
    selections: &[String],
    voices: &'a [CuratedVoice],
) -> Result<Vec<&'a CuratedVoice>, String> {
    let mut selected = vec![false; voices.len()];
    let mut order = Vec::new();
    for token in selections
        .iter()
        .flat_map(|selection| {
            selection.split(|character: char| character == ',' || character.is_whitespace())
        })
        .filter(|token| !token.is_empty())
    {
        if let Some((first, last)) = numeric_range(token) {
            if first == 0 || first > last || last > voices.len() {
                return Err(format!("catalog selection '{token}' is out of range"));
            }
            for number in first..=last {
                select(number - 1, &mut selected, &mut order);
            }
            continue;
        }
        if token.bytes().all(|byte| byte.is_ascii_digit()) {
            let number = token
                .parse::<usize>()
                .map_err(|_| format!("catalog selection '{token}' is invalid"))?;
            if number == 0 || number > voices.len() {
                return Err(format!("catalog selection '{token}' is out of range"));
            }
            select(number - 1, &mut selected, &mut order);
            continue;
        }
        return Err(format!(
            "catalog selection '{token}' is invalid; use a number or numeric range"
        ));
    }
    if order.is_empty() {
        return Err("voice installation requires at least one selection".to_owned());
    }
    Ok(order.into_iter().map(|index| &voices[index]).collect())
}

fn numeric_range(value: &str) -> Option<(usize, usize)> {
    let (first, last) = value.split_once('-')?;
    if first.is_empty()
        || last.is_empty()
        || !first.bytes().all(|byte| byte.is_ascii_digit())
        || !last.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some((first.parse().ok()?, last.parse().ok()?))
}

fn select(index: usize, selected: &mut [bool], order: &mut Vec<usize>) {
    if !selected[index] {
        selected[index] = true;
        order.push(index);
    }
}

fn show_pages(
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    prompts: &mut dyn Write,
    voices: &[CuratedVoice],
    installed: &[bool],
) -> Result<bool, String> {
    if installed.len() != voices.len() {
        return Err("curated catalog state is inconsistent".to_owned());
    }
    writeln!(output, "Pocket TTS voice catalog:")
        .map_err(|_| "could not write curated voice catalog".to_owned())?;
    for (page, chunk) in voices.chunks(PAGE_ITEMS).enumerate() {
        let start = page * PAGE_ITEMS;
        for (offset, voice) in chunk.iter().enumerate() {
            let index = start + offset;
            let status = if installed[index] {
                " · installed"
            } else {
                ""
            };
            writeln!(
                output,
                "{:>2}. {} · {} · {}{status}",
                index + 1,
                voice.name,
                voice.collection,
                voice.license_id,
            )
            .map_err(|_| "could not write curated voice catalog".to_owned())?;
        }
        output
            .flush()
            .map_err(|_| "could not write curated voice catalog".to_owned())?;
        if start + chunk.len() == voices.len() {
            break;
        }
        prompts
            .write_all(b"-- more -- (Enter to continue, q to stop) ")
            .and_then(|()| prompts.flush())
            .map_err(|_| "could not write catalog pager".to_owned())?;
        let mut answer = String::new();
        input
            .read_line(&mut answer)
            .map_err(|_| "could not read catalog pager".to_owned())?;
        if answer.trim().eq_ignore_ascii_case("q") {
            return Ok(false);
        }
    }
    Ok(true)
}

fn render_catalog(
    output: &mut dyn Write,
    voices: &[CuratedVoice],
    installed: &[bool],
) -> Result<(), String> {
    writeln!(output, "Pocket TTS voice catalog:")
        .map_err(|_| "could not write curated voice catalog".to_owned())?;
    for (index, voice) in voices.iter().enumerate() {
        let status = if installed[index] {
            " · installed"
        } else {
            ""
        };
        writeln!(
            output,
            "{:>2}. {} · {} · {}{status}",
            index + 1,
            voice.name,
            voice.collection,
            voice.license_id,
        )
        .map_err(|_| "could not write curated voice catalog".to_owned())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use utterpipe_pocket_tts::voice::CURATED_VOICES;

    use super::*;

    #[test]
    fn numbers_ranges_and_duplicates_resolve_in_user_order() {
        let selected = resolve_selections(
            &["2,4-5".to_owned(), "1".to_owned(), "2".to_owned()],
            CURATED_VOICES,
        )
        .unwrap();
        let ids: Vec<_> = selected.into_iter().map(|voice| voice.id).collect();
        assert_eq!(
            ids,
            [
                CURATED_VOICES[1].id,
                CURATED_VOICES[3].id,
                CURATED_VOICES[4].id,
                CURATED_VOICES[0].id,
            ]
        );
        assert!(resolve_selections(&["0".to_owned()], CURATED_VOICES).is_err());
        assert!(resolve_selections(&["3-2".to_owned()], CURATED_VOICES).is_err());
        let error =
            resolve_selections(&[CURATED_VOICES[0].id.to_owned()], CURATED_VOICES).unwrap_err();
        assert!(error.contains("use a number or numeric range"));
    }

    #[test]
    fn pager_numbers_items_and_can_stop_after_one_page() {
        let mut input = Cursor::new(b"q\n");
        let mut output = Vec::new();
        let mut prompts = Vec::new();
        let complete = show_pages(
            &mut input,
            &mut output,
            &mut prompts,
            CURATED_VOICES,
            &vec![false; CURATED_VOICES.len()],
        )
        .unwrap();

        assert!(!complete);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains(" 1."));
        assert!(output.contains(" 8."));
        assert!(!output.contains(" 9."));
        assert!(!output.contains(CURATED_VOICES[0].id));
        assert!(!output.contains("available"));
        assert!(String::from_utf8(prompts).unwrap().contains("-- more --"));
    }
}
