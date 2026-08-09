# Security Policy

## Supported versions

Security fixes are applied to the latest published Kallisti release. Older builds may not receive backports.

## Reporting a vulnerability

Do not open a public issue containing exploit details, credentials, private URLs, device identifiers, personal data, or production logs.

Use GitHub's private vulnerability reporting feature for this repository. Include:

- affected version and component;
- impact and attack prerequisites;
- minimal reproduction steps using sanitized data;
- suggested mitigation, if known.

## Security model

### iOS app

- Credentials are stored in the iOS Keychain.
- Gateway passwords are used only for authentication and are not embedded in source or release artifacts.
- Native gateway sessions use authenticated WebSocket tickets or validated cookie sessions.
- Camera, microphone, location, motion, health, and notification permissions are requested only for features that need them.

### Connector

- The connector runs beside the operator's Hermes Agent installation.
- Mobile HTTP endpoints require a validated gateway session, native bearer, or paired-device credential as appropriate.
- Paired-device credentials survive connector restarts through the local protected registry.
- Pairing codes are time-limited and persisted only as digests with installation binding.
- Sensor and delivery state remain on operator-controlled infrastructure.

### Native media

The `/v1/native/media` endpoint:

- requires authentication;
- serves only supported image files under configured Hermes image/media roots;
- rejects arbitrary paths, traversal, unsupported types, missing files, and files over 10 MB;
- delegates gateway cookie and bearer validation to the gateway identity endpoint;
- uses private cache headers.

### Deployment

Operators are responsible for:

- TLS termination and reverse-proxy access controls;
- strong gateway and connector credentials;
- Apple signing keys, provisioning profiles, and APNs credentials;
- filesystem permissions on connector state and Hermes media directories;
- log retention and removal of personal or secret data before sharing diagnostics.

## Public repository hygiene

Never commit:

- `.env` files, API keys, passwords, refresh tokens, signing keys, or provisioning profiles;
- private IP addresses, internal hostnames, user-specific absolute paths, device IDs, or personal email addresses;
- production database files, connector state, pairing registries, screenshots, or raw protocol captures;
- signed IPAs or archives containing private entitlements or profiles.

Use placeholders and documentation examples instead.
