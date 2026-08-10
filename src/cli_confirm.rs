use std::io::{self, BufRead, IsTerminal, Write};

use utterpipe_pocket_tts::model::LICENSE_IDS;

const PREPARE_CONFIRMATION_REQUIRED: &str = "preparation requires --yes after reviewing the plan";
const LICENSE_ACCEPTANCE_REQUIRED: &str = "all three disclosure IDs must be supplied with --accept";
const VOICE_CONSENT_REQUIRED: &str =
    "voice import requires --consent-confirmed after reviewing the plan";
const REMOVAL_CONFIRMATION_REQUIRED: &str = "removal requires --yes";

pub(crate) fn prepare(accepted: &mut Vec<String>, yes: bool) -> Result<(), String> {
    with_terminal(|input, output, interactive| {
        prepare_with(input, output, interactive, accepted, yes)
    })
}

pub(crate) fn voice_import(consent_confirmed: bool) -> Result<bool, String> {
    with_terminal(|input, output, interactive| {
        voice_import_with(input, output, interactive, consent_confirmed)
    })
}

pub(crate) fn removal(yes: bool, artifact: &str) -> Result<(), String> {
    with_terminal(|input, output, interactive| {
        removal_with(input, output, interactive, yes, artifact)
    })
}

fn with_terminal<T>(
    action: impl FnOnce(&mut dyn BufRead, &mut dyn Write, bool) -> Result<T, String>,
) -> Result<T, String> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let stderr = io::stderr();
    // The plan and disclosures are printed on stdout while prompts use stderr.
    // Requiring all three streams to be terminals ensures that a human can see
    // what they are accepting and prevents redirected/piped commands from
    // consuming input as authorization.
    let interactive = stdin.is_terminal() && stdout.is_terminal() && stderr.is_terminal();
    let mut input = stdin.lock();
    let mut output = stderr.lock();
    action(&mut input, &mut output, interactive)
}

fn prepare_with(
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    interactive: bool,
    accepted: &mut Vec<String>,
    yes: bool,
) -> Result<(), String> {
    if !yes && !interactive {
        return Err(PREPARE_CONFIRMATION_REQUIRED.to_owned());
    }

    for required in LICENSE_IDS {
        if accepted.iter().any(|value| value == required) {
            continue;
        }
        if !interactive {
            return Err(LICENSE_ACCEPTANCE_REQUIRED.to_owned());
        }
        let prompt = format!("Acknowledge and accept disclosure '{required}'? [y/N] ");
        if !confirm(input, output, &prompt)? {
            return Err("model disclosure acceptance declined".to_owned());
        }
        accepted.push((*required).to_owned());
    }

    if !yes && !confirm(input, output, "Install the Pocket TTS model now? [y/N] ")? {
        return Err("model preparation cancelled".to_owned());
    }
    Ok(())
}

fn voice_import_with(
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    interactive: bool,
    consent_confirmed: bool,
) -> Result<bool, String> {
    if consent_confirmed {
        return Ok(true);
    }
    if !interactive {
        return Err(VOICE_CONSENT_REQUIRED.to_owned());
    }
    if confirm(
        input,
        output,
        "Confirm that you have the necessary rights and consent to use this reference voice? [y/N] ",
    )? {
        Ok(true)
    } else {
        Err("voice consent confirmation declined".to_owned())
    }
}

fn removal_with(
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    interactive: bool,
    yes: bool,
    artifact: &str,
) -> Result<(), String> {
    if yes {
        return Ok(());
    }
    if !interactive {
        return Err(REMOVAL_CONFIRMATION_REQUIRED.to_owned());
    }
    if confirm(input, output, &format!("Remove {artifact}? [y/N] "))? {
        Ok(())
    } else {
        Err("removal cancelled".to_owned())
    }
}

fn confirm(input: &mut dyn BufRead, output: &mut dyn Write, prompt: &str) -> Result<bool, String> {
    output
        .write_all(prompt.as_bytes())
        .and_then(|()| output.flush())
        .map_err(|_| "could not write interactive prompt".to_owned())?;

    let mut answer = String::new();
    input
        .read_line(&mut answer)
        .map_err(|_| "could not read interactive confirmation".to_owned())?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn interactive_prepare_collects_each_missing_license_then_confirms() {
        let mut accepted = vec![LICENSE_IDS[0].to_owned()];
        let mut input = Cursor::new(b"y\nyes\ny\n");
        let mut output = Vec::new();

        prepare_with(&mut input, &mut output, true, &mut accepted, false).unwrap();

        assert!(
            LICENSE_IDS
                .iter()
                .all(|id| accepted.iter().any(|value| value == id))
        );
        let prompts = String::from_utf8(output).unwrap();
        assert!(!prompts.contains(LICENSE_IDS[0]));
        assert!(prompts.contains(LICENSE_IDS[1]));
        assert!(prompts.contains(LICENSE_IDS[2]));
        assert!(prompts.contains("Install the Pocket TTS model now?"));
    }

    #[test]
    fn explicit_prepare_flags_do_not_prompt() {
        let mut accepted = LICENSE_IDS.iter().map(ToString::to_string).collect();
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        prepare_with(&mut input, &mut output, false, &mut accepted, true).unwrap();

        assert!(output.is_empty());
    }

    #[test]
    fn non_interactive_prepare_fails_even_when_yes_is_piped() {
        let mut accepted = Vec::new();
        let mut input = Cursor::new(b"yes\nyes\nyes\nyes\n");
        let mut output = Vec::new();

        let error = prepare_with(&mut input, &mut output, false, &mut accepted, false).unwrap_err();

        assert_eq!(error, PREPARE_CONFIRMATION_REQUIRED);
        assert!(accepted.is_empty());
        assert_eq!(input.position(), 0);
        assert!(output.is_empty());
    }

    #[test]
    fn interactive_prepare_decline_is_fail_closed() {
        let mut accepted = Vec::new();
        let mut input = Cursor::new(b"n\n");
        let mut output = Vec::new();

        let error = prepare_with(&mut input, &mut output, true, &mut accepted, true).unwrap_err();

        assert_eq!(error, "model disclosure acceptance declined");
        assert!(accepted.is_empty());
    }

    #[test]
    fn voice_consent_is_prompted_only_for_an_interactive_human() {
        let mut interactive_input = Cursor::new(b" YES \n");
        let mut interactive_output = Vec::new();
        assert!(
            voice_import_with(&mut interactive_input, &mut interactive_output, true, false,)
                .unwrap()
        );
        assert!(
            String::from_utf8(interactive_output)
                .unwrap()
                .contains("necessary rights and consent")
        );

        let mut piped_input = Cursor::new(b"yes\n");
        let mut piped_output = Vec::new();
        assert_eq!(
            voice_import_with(&mut piped_input, &mut piped_output, false, false).unwrap_err(),
            VOICE_CONSENT_REQUIRED
        );
        assert_eq!(piped_input.position(), 0);
        assert!(piped_output.is_empty());
    }

    #[test]
    fn removal_defaults_to_no_on_empty_input() {
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        assert_eq!(
            removal_with(&mut input, &mut output, true, false, "voice:sample").unwrap_err(),
            "removal cancelled"
        );
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("Remove voice:sample?")
        );
    }
}
