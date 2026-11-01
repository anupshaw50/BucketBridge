# Security policy

## Supported versions

Only the latest release receives security fixes.

## Reporting a vulnerability

Please do not open a public issue. Use GitHub private vulnerability reporting:
open the repository's **Security** tab and choose **Report a vulnerability**.

Include:

- the BucketBridge version and operating system, and whether it is the app or `bb`
- a description of the issue and its impact
- steps or a proof of concept to reproduce it
- any logs or files, with secrets removed

## What to expect

You should get an acknowledgement within 7 days and a first assessment within
14 days. Fixes are released as soon as they are ready, and you will be credited
in the advisory unless you prefer not to be.

## Scope

Of most interest:

- handling of secret keys, the keychain store and the master-password vault
- cryptography (key derivation, encryption formats)
- path traversal and unsafe file writes during downloads, sync and folder expansion
- the desktop IPC surface and the webview content security policy

Out of scope: issues that need an already compromised machine or account, and
problems in third-party S3 servers.
