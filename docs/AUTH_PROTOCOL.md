# Native KAIST authentication protocol

This document records the narrow protocol surface implemented by `klms`. It is
an interoperability description, not a general KAIST SSO client API.

## Origins and lifetime

The login transport is created for one command and permits only
`https://sso.kaist.ac.kr` and `https://klms.kaist.ac.kr`. It follows at most
eight redirects, rejects URL userinfo and unrelated origins, bounds response
bodies to 1 MiB, and holds cookies only in memory. Loopback HTTP is accepted
solely by integration tests.

The entry request identifies the KLMS SSO agent (`kaist-prod-klms`). Login init
returns hexadecimal key material. The browser-compatible request payload is
JSON encrypted with KISA SEED in CBC mode and ANSI X9.23 padding; only the
ciphertext hex is sent as `user_data`. Login keys and cleartext payloads are
zeroized after use.

## Easy Login

1. Initialize SSO and encrypt the prompted login identifier.
2. Start `/auth/twofactor/mfa/init`, then enter the Easy Login challenge view.
3. Show the confirmation code when supplied and poll the authorization endpoint
   every three seconds for at most three minutes.
4. Submit trusted-device identifiers to the policy check and follow the link
   transition back to KLMS.

Pending, cancelled, expired, mismatched, temporarily blocked, permanently
blocked, and unregistered-app responses are distinct failures.

## Password login

1. Prompt for the identifier and a hidden password.
2. Encrypt the primary-login JSON, including any previously saved trusted-device
   identifiers, and submit it to SSO.
3. When second factor is required, request either external email or SMS
   delivery, prompt for a hidden six-digit code, and verify it.
4. Follow the link transition back to KLMS.

Invalid credentials, lockouts, invalid/expired requests, delivery failures,
invalid codes, expired codes, and attempt exhaustion remain distinct errors.
Password-update screens are surfaced as actionable authentication failures.
When KAIST requires first-time device registration, `klms` validates the
session-bound registration actions, registers the current native client, and
continues the link automatically. The resulting trusted-device identifier is
retained for later login policy checks.

## Persistent boundary

After a successful link, only cookie pairs issued by the KLMS host at `/` and
trusted-device identifiers are retained. General KAIST SSO cookies, passwords,
verification codes, encryption keys, raw HTML, and Moodle `sesskey` values are
discarded. The versioned session file is atomically written with private Unix
permissions. `auth logout` deletes this file only.

All known result codes are mapped in `src/auth/codes.rs`. Any unknown result
fails closed as `AUTH_PROTOCOL_CHANGED` so upstream changes cannot silently
fall through into a misleading success.
