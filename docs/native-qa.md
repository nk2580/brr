# Native app QA guide

The native build environment may be unavailable while the Mac is locked. This fixture prepares a small, isolated coding task for a later manual native-app run. Generating the fixture only verifies that the local Go baseline is red; it does not verify the native app, agent, approval UI, or ChatGPT authentication.

## Prepare a fresh fixture

From the repository root, run:

```sh
node scripts/prepare-native-qa.mjs
```

The script creates a unique directory under `.cache/native-qa/<id>/`. It never reuses or deletes a prior fixture. The Go workspace is `<fixture>/workspace`; the controlled approval target is its sibling at `<fixture>/approval-target`. The workspace contains an intentionally incorrect `Add` function, table tests, and a local Git baseline commit with hooks and signing disabled and no remote. Its initial `./test.sh` is expected to exit with status 1 and report `--- FAIL: TestAdd`.

Use the workspace's `./test.sh` for both the baseline and the agent's verification. It sets `GOCACHE` to `.cache/go-build` inside the workspace, and sets `GOTOOLCHAIN=local`, `GOPROXY=off`, `GOSUMDB=off`, `GOWORK=off`, `GOENV=off`, and an empty `GOFLAGS`. The workspace `.gitignore` excludes that cache. Do not substitute a plain `go test` command, since it can use machine-level Go settings or caches.

Record the exact paths printed by the script. Do not move either directory during the run.

## Run the native coding flow

1. Launch the actual Brr native desktop app and open the printed `workspace` path. Complete ChatGPT sign-in through the app and verify the signed-in account and model selector appear. Use the real account flow; do not inject credentials, tokens, environment overrides, or mocked authentication state.
2. Ask the agent to inspect `TASK.md`, fix `Add`, run `./test.sh`, and show the resulting diff. Keep the original test cases/assertions and the isolated `test.sh` runner intact. Confirm the failing baseline becomes green and inspect the change with `git diff`; reject a green result caused by weakening/deleting tests or modifying the runner. The expected implementation is addition, and the tests should pass without downloading dependencies.
3. Check the native window visually: title bar, project/workspace display, transcript, composer, account/model controls, and activity/approval surfaces should be usable at the current window size. This check must be made in the native app rather than inferred from browser preview.
4. Run the deterministic Stop probe below. The Go tests finish too quickly to exercise Stop reliably. Then quit the app fully, relaunch it, reopen the same workspace/thread, and verify account state and conversation history resume correctly.

## Verify Stop with a deterministic command

In the fixture workspace, ask the agent to run this exact foreground shell command:

```sh
sleep 30; printf finished > stop-probe.txt
```

Ask the agent to keep waiting for this foreground command to finish; it must not background it or return an early final answer. As soon as the app shows the command running, click the actual Stop control. Independently wait 35 seconds from the stop action. Confirm the turn is interrupted and `stop-probe.txt` does not exist. If the app never showed a running command, no actual interrupt occurred, the marker appears, or the turn completes normally, record Stop as failed or unverified as appropriate; do not infer success from an interrupted-looking UI alone.

## Verify write approval outside the workspace

The script prints the unique sibling path ending in `approval-target/approval-marker.txt`. It must be absent at the start of this exercise.

1. Ask the agent to write a harmless one-line marker to that exact path. Wait for the native app to display its real approval request and deny it. Check that `approval-marker.txt` was not created and that `approval-target/README.md` is unchanged.
2. Ask again for the same write, wait for the real approval request, and allow it. Check that the marker now exists with the requested content.

If the app never presents a real approval request, record approval behavior as **unverified**. Do not simulate the prompt, alter permission state behind the app, or count a tool's claimed success as an approval check. The marker is a harmless fixture artifact; retain it with its unique fixture directory for review.

## Record the outcome

Record the fixture ID, native app build, account/model shown (without recording credentials), baseline and post-fix test results, `git diff` review, approval denial/allow results, Stop result, relaunch/resume result, and visual findings. Separate anything not exercised from a pass. A generated fixture and red baseline alone are not evidence that the native coding or authentication flow passed.
