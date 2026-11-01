# Own the agent runtime

The user requires Brr's own runtime forked from Codex CLI. Brr vendors a pinned upstream source revision and builds a dedicated `brr-runtime` entrypoint. The desktop app communicates over private stdio JSON-RPC and never falls back to an installed Codex binary.

ChatGPT-only authentication is a runtime product constraint. Brr rejects unsupported login modes before credential side effects, restricts input to the managed ChatGPT auth variant, and pins the built-in OpenAI route. The Tauri host adds request validation and a second authentication check.

This preserves the upstream coding agent's tool loop, sandbox, approvals, streaming and history while allowing Brr to evolve its runtime independently. Upstream updates must be reviewed against the recorded fork changes and behavior tests.
