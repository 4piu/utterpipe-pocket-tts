use std::io::{self, BufRead, IsTerminal, Write};

use utterpipe_pocket_tts::voice::CuratedLicense;
use utterpipe_pocket_tts::xn_bundle::BundleLicense;

const VOICE_CONSENT_REQUIRED: &str =
    "voice import requires --consent-confirmed after reviewing the plan";
const REMOVAL_CONFIRMATION_REQUIRED: &str = "removal requires --yes";

pub(crate) fn voice_import(consent_confirmed: bool) -> Result<bool, String> {
    with_terminal(|input, output, interactive| {
        voice_import_with(input, output, interactive, consent_confirmed)
    })
}

pub(crate) fn curated_voice_install(
    licenses: &[CuratedLicense],
    accepted: &mut Vec<String>,
    yes: bool,
) -> Result<(), String> {
    with_terminal(|input, output, interactive| {
        curated_voice_install_with(input, output, interactive, licenses, accepted, yes)
    })
}

pub(crate) fn model_bundle_import(
    licenses: &[BundleLicense],
    accepted: &mut Vec<String>,
    yes: bool,
) -> Result<(), String> {
    with_terminal(|input, output, interactive| {
        model_bundle_import_with(input, output, interactive, licenses, accepted, yes)
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

fn curated_voice_install_with(
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    interactive: bool,
    licenses: &[CuratedLicense],
    accepted: &mut Vec<String>,
    yes: bool,
) -> Result<(), String> {
    if !yes && !interactive {
        return Err(
            "curated voice installation requires --yes after reviewing the plan".to_owned(),
        );
    }
    for license in licenses {
        if accepted.iter().any(|value| value == license.id) {
            continue;
        }
        if !interactive {
            return Err(format!(
                "curated voice installation requires --accept {}",
                license.id
            ));
        }
        if !confirm(
            input,
            output,
            &format!(
                "Acknowledge upstream license '{}' and confirm permitted, consented use? [y/N] ",
                license.id
            ),
        )? {
            return Err("curated voice license acknowledgement declined".to_owned());
        }
        accepted.push(license.id.to_owned());
    }
    if !yes
        && !confirm(
            input,
            output,
            "Download and install this curated voice? [y/N] ",
        )?
    {
        return Err("curated voice installation cancelled".to_owned());
    }
    Ok(())
}

fn model_bundle_import_with(
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    interactive: bool,
    licenses: &[BundleLicense],
    accepted: &mut Vec<String>,
    yes: bool,
) -> Result<(), String> {
    if !yes && !interactive {
        return Err("model bundle import requires --yes after reviewing the plan".to_owned());
    }
    for license in licenses {
        if accepted.iter().any(|value| value == &license.id) {
            continue;
        }
        if !interactive {
            return Err(format!(
                "model bundle import requires --accept {}",
                license.id
            ));
        }
        if !confirm(
            input,
            output,
            &format!("Acknowledge model disclosure '{}'? [y/N] ", license.id),
        )? {
            return Err("model bundle disclosure acceptance declined".to_owned());
        }
        accepted.push(license.id.clone());
    }
    if !yes && !confirm(input, output, "Install this XN model bundle now? [y/N] ")? {
        return Err("model bundle import cancelled".to_owned());
    }
    Ok(())
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
    use utterpipe_pocket_tts::voice::{CC_BY_LICENSE, CC0_LICENSE};

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

    #[test]
    fn curated_voice_install_collects_license_and_confirmation_interactively() {
        let mut accepted = Vec::new();
        let mut input = Cursor::new(b"yes\ny\ny\n");
        let mut output = Vec::new();

        curated_voice_install_with(
            &mut input,
            &mut output,
            true,
            &[CC0_LICENSE, CC_BY_LICENSE],
            &mut accepted,
            false,
        )
        .unwrap();

        assert_eq!(accepted, [CC0_LICENSE.id, CC_BY_LICENSE.id]);
        let prompts = String::from_utf8(output).unwrap();
        assert!(prompts.contains("confirm permitted, consented use"));
        assert!(prompts.contains("Download and install"));
    }

    #[test]
    fn model_bundle_import_collects_manifest_disclosures() {
        let licenses = vec![BundleLicense {
            id: "cc-by-4.0".to_owned(),
            name: "CC BY 4.0".to_owned(),
            url: "https://creativecommons.org/licenses/by/4.0/".to_owned(),
            requires_acceptance: true,
        }];
        let mut accepted = Vec::new();
        let mut input = Cursor::new(b"yes\nyes\n");
        let mut output = Vec::new();
        model_bundle_import_with(
            &mut input,
            &mut output,
            true,
            &licenses,
            &mut accepted,
            false,
        )
        .unwrap();
        assert_eq!(accepted, ["cc-by-4.0"]);
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("Install this XN model bundle")
        );

        let mut accepted = Vec::new();
        let mut input = Cursor::new(b"yes\nyes\n");
        let mut output = Vec::new();
        assert!(
            model_bundle_import_with(
                &mut input,
                &mut output,
                false,
                &licenses,
                &mut accepted,
                false,
            )
            .is_err()
        );
        assert_eq!(input.position(), 0);
    }

    #[test]
    fn curated_voice_install_flags_are_explicit_for_automation() {
        let mut accepted = vec![CC0_LICENSE.id.to_owned(), CC_BY_LICENSE.id.to_owned()];
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        curated_voice_install_with(
            &mut input,
            &mut output,
            false,
            &[CC0_LICENSE, CC_BY_LICENSE],
            &mut accepted,
            true,
        )
        .unwrap();
        assert!(output.is_empty());
    }
}
