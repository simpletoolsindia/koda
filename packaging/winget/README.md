# winget

Three manifests, as the winget schema requires: version, installer, and
`en-US` locale. koda ships as a **portable** package — the zip holds `koda.exe`,
winget puts it in its own links directory and adds that to PATH, so there is no
installer to run and nothing to uninstall but a file.

`Moniker: koda` in the locale manifest is the line that makes the short command
work:

```powershell
winget install koda                    # via the moniker
winget install SimpleToolsIndia.Koda   # the full identifier
```

## Making `winget install koda` work

Unlike homebrew-core, winget has **no notability requirement** — a valid
manifest that passes automated validation gets merged.

**Status: submitted.**
[microsoft/winget-pkgs#432036](https://github.com/microsoft/winget-pkgs/pull/432036)
adds these three files at `manifests/s/SimpleToolsIndia/Koda/0.1.0/`. It is
labelled `Needs-CLA`: the repository owner has to accept Microsoft's Contributor
License Agreement by commenting on the PR, and nobody can do that on their
behalf:

```text
@microsoft-github-policy-service agree
```

Once the CLA is accepted and validation passes, `winget install koda` works for
everyone.

For future versions, the submission flow is:

```powershell
winget install wingetcreate            # Microsoft's manifest tool

# Validate locally first (needs Windows):
winget validate --manifest packaging\winget
winget install --manifest packaging\winget   # installs from the local manifest

# Then submit. wingetcreate forks winget-pkgs, commits to
# manifests/s/SimpleToolsIndia/Koda/<version>/ and opens the PR:
wingetcreate submit --token <github-token> packaging\winget
```

For later releases, `wingetcreate update SimpleToolsIndia.Koda --version 0.2.0
--urls <new zip url>` does the whole thing in one command.

## Keeping it current

```sh
packaging/update.py v0.2.0            # rewrite the manifests from that release
packaging/update.py v0.2.0 --check    # CI: fail if it was forgotten
```

## What was verified here, and what was not

The manifests parse, carry the schema each type requires, and the
`InstallerSha256` was checked against the actual published zip. `winget validate`
and an end-to-end install need Windows, and have not been run.
