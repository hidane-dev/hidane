# macOS signing and notarization

The macOS binaries of hidane are not signed with a Developer ID or notarized by Apple. Signing is
deferred: this page records what users see meanwhile, and what the project would need to sign
and notarize the release binaries, so that the decision has a starting point. Nothing here is
implemented.

## What users see today

macOS checks a program with Gatekeeper only when the file carries the quarantine attribute
(`com.apple.quarantine`), which browsers and some other apps add to what they download. Command
line downloads do not add it, so most channels are not affected:

| Channel | Quarantined | Runs as is |
|---|---|---|
| `brew install hidane-dev/tap/hidane` | No (a formula, downloaded with `curl`) | Yes |
| `curl -fsSL https://hidane.dev/install.sh \| sh`, `curl` or `gh release download` | No | Yes |
| `npm install hidane` (and the `@hidane-dev/darwin-*` binaries) | No | Yes |
| `cargo install hidane`, `cargo binstall hidane` | No | Yes |
| `dart pub global activate hidane` (the launcher downloads the binary) | No | Yes |
| setup-hidane on a macOS runner | No | Yes |
| An archive downloaded with a browser | Yes | No: Gatekeeper refuses to open it |

For a browser download, remove the attribute after unpacking, once you have checked the archive
([verifying-releases.md](verifying-releases.md)):

```sh
xattr -d com.apple.quarantine hidane
```

After the first refusal, *System Settings → Privacy & Security* also offers to open the blocked
file anyway, one file at a time.

The arm64 binary carries the ad-hoc signature that Apple's linker adds to every arm64 executable
(`codesign -dv` shows `Signature=adhoc`, `linker-signed`); it says nothing about who built it.
The x86_64 binary is not signed at all.

## What signing would need

| Need | Detail |
|---|---|
| Apple Developer Program membership | 99 USD a year. An organization enrollment (in the name of a legal entity, with a D-U-N-S number) shows the organization as the developer; an individual enrollment shows the person's name. The membership must stay active to create certificates and to notarize |
| A Developer ID Application certificate | Created by the membership's Account Holder (up to five per membership), exported with its private key as a `.p12` file with a password. Software already signed keeps running if the membership lapses; signing new releases needs an active membership once the certificate expires |
| An App Store Connect API key for `notarytool` | A team key (Issuer ID, Key ID and the `.p8` private key, which can be downloaded once) with the Developer role, so that the workflow notarizes without an Apple ID password |
| A macOS runner | `codesign` and `xcrun notarytool` ship with Xcode on GitHub's macOS runners |

### GitHub secrets

In a dedicated environment (`macos-signing`, say) that accepts tags `v*` only and holds no other
secrets, so that pull requests and branches never see them (required reviewers on it would add a
second approval to each release, before the draft):

| Secret | Content |
|---|---|
| `MACOS_CERTIFICATE_P12` | The Developer ID Application certificate and its key, the `.p12` file in base64 |
| `MACOS_CERTIFICATE_PASSWORD` | The `.p12` file's password |
| `APPLE_API_KEY_P8` | The App Store Connect API key's `.p8` file, as text |
| `APPLE_API_KEY_ID` | Its Key ID |
| `APPLE_API_ISSUER_ID` | Its Issuer ID |

The signing identity, `Developer ID Application: <name> (<team ID>)`, is not secret and can be a
repository variable. The temporary keychain's password is generated in the job.

### Release workflow steps

A job `sign-macos` in `.github/workflows/release.yml`, after `build` and before the release's
assets are gathered, on `macos-14`, with `environment: macos-signing` and only `contents: read`,
for each macOS target, on tags only:

1. Download the build's archive artifact and unpack the binary.
2. Import the certificate into a temporary keychain:
   `security create-keychain`, `security set-keychain-settings -lut 21600`,
   `security unlock-keychain`, `security import cert.p12 -P "$MACOS_CERTIFICATE_PASSWORD" -T /usr/bin/codesign`,
   `security set-key-partition-list -S apple-tool:,apple: -s`, and `security list-keychains -d user -s`
   with the temporary keychain first.
3. Sign with the hardened runtime and a secure timestamp (both required for notarization):

   ```sh
   codesign --force --options runtime --timestamp \
     --identifier dev.hidane.hidane \
     --sign "Developer ID Application: <name> (<team ID>)" hidane
   codesign --verify --strict --verbose=2 hidane
   ./hidane --version
   ```

   hidane needs no entitlements: it does not generate code at run time or load other code.
4. Notarize. The notary service accepts ZIP archives, flat installer packages and disk images, not
   `.tar.gz` files or bare executables, so the binary goes up in a ZIP made for the purpose:

   ```sh
   ditto -c -k --keepParent hidane notarize.zip
   xcrun notarytool submit notarize.zip \
     --key AuthKey.p8 --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER_ID" \
     --wait --timeout 30m
   ```

   Fail the job unless the status is `Accepted`, and keep `xcrun notarytool log <submission id>`
   in the job summary either way.
5. Pack the signed binary into the release archive again (`packaging/release/archive.py`) and
   upload it in place of the unsigned one. The workflow then writes `sha256sums.txt` and attests
   the archives as it does now, so the checksums, the provenance and the SBOMs describe the signed
   archives.
6. Delete the temporary keychain and the key files (`if: always()`).

**Stapling.** A notarization ticket can be stapled to an app bundle, a disk image or an installer
package, not to a bare executable: Apple's documentation says tickets are created for standalone
binaries but cannot be stapled to them, and a ZIP or `.tar.gz` cannot carry one either. Gatekeeper
then looks the ticket up online the first time a quarantined binary runs; offline, that check
cannot succeed. Shipping a stapled ticket would mean a signed installer package (`.pkg`, with a
Developer ID Installer certificate as well) or a disk image, a new kind of asset that each
channel would have to learn about.

## What it would change

- A browser download would run without `xattr` once the ticket lookup succeeds.
- The macOS binaries would no longer be byte-for-byte rebuildable: the signature (with Apple's
  timestamp) is part of the file. A rebuild could still be compared after removing it
  (`codesign --remove-signature`), and the comparison would need checking before relying on it
  ([reproducible-builds.md](reproducible-builds.md)).
- Releases would depend on Apple's notary service, which can be slow or down; the job would need
  a retry path that does not hold up the other platforms.
- The membership, the certificate (and its renewal) and the API key become things a maintainer
  has to keep, and the certificate is tied to whoever enrolled.
