# Contributing to Brr

Brr is an early, maintainer-led open source project. Contributions are welcome
through focused pull requests. There is no foundation, contributor agreement,
or established review group at this stage.

Before making a large change, open an issue or discussion describing the user
problem and proposed shape. For a pull request, explain the behavior change,
keep the patch focused, preserve existing license notices, and include the
checks relevant to the affected area. Do not include credentials, private
workspace data, runtime transcripts, generated binaries, or local QA evidence.

The desktop host and owned runtime must keep inference restricted to managed
ChatGPT sign-in and the built-in OpenAI provider. Do not add an installed CLI
fallback or expose API-key or alternate-provider inference. Changes to the
Codex source fork must stay narrow and be recorded in `runtime/UPSTREAM.md`.

The runtime includes upstream support for Skills and MCP servers. Brr does not
currently provide an in-app marketplace or setup flow for them; contributors
should describe this as runtime support and document any manual configuration
that a feature requires.

Please retain the license and attribution files when copying or redistributing
source. By submitting a contribution, you agree that it is provided under the
Apache License 2.0 in `LICENSE`; no separate CLA is required.
