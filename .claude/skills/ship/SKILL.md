---
name: ship
description: >
  Cut an Oatmeal release. Use when the user says ship, cut a release, publish a
  version, tag a release, or "release X.Y.Z". Bumps the version everywhere it
  lives, commits, and hands over the exact merge-and-tag commands. Knows the two
  traps that have bitten every release so far.
---

# Shipping a release

There is no release server. Pushing a tag matching `v*` runs
`.github/workflows/release.yml`, which tests, builds the bundle, and attaches
`Oatmeal-<version>-apple-silicon.dmg` to a GitHub release. The running app asks
the releases API for the newest tag on launch (`update.rs`), so a tag *is* the
release announcement.

## The two traps

Both have already cost a cancelled build. Do not skip the checks that catch them.

1. **The tag lands on a stale `main`.** The local checkout sits behind
   `origin/main`, so `git tag vX.Y.Z` tags the *old* commit and CI builds the
   wrong version. Happened on v1.1.2 and v1.2.0. Push `main` **before** tagging,
   then verify the tag points at the bump commit before pushing it. Deleting a
   pushed tag does **not** delete the GitHub release object, so catching this
   before the build finishes matters.
2. **The version is already released.** Reusing a version collides with an
   existing release. Always check the published tags first — do not trust
   `tauri.conf.json` to tell you what is unreleased, because it holds the
   version that was *last shipped*.

## Step 1 — Decide the version

Read the current version and every published tag:

```sh
grep -m1 '^version' app/src-tauri/Cargo.toml
git tag --list 'v*' --sort=-v:refname | head -5
curl -fsSL -H "User-Agent: Oatmeal" https://api.github.com/repos/Cujoqt/oatmeal/releases/latest | grep '"tag_name"'
```

If the version in `Cargo.toml` already appears as a published tag, the next
release is a **bump from it**, not a re-use of it. Semver against what actually
changed: new features → minor, fixes only → patch.

**State the chosen version and why before touching a file.** If the user named a
version that is already published, say so and stop — that is the trap above, and
it is exactly the kind of thing a human means to be corrected on.

## Step 2 — Preflight

All of these must pass. Report the actual output; do not claim a check you did
not run.

```sh
git status --short                    # must be empty
cd app/src-tauri && cargo test --lib  # must be all green
```

Anything failing is a stop, not a warning.

## Step 3 — Bump the version everywhere

Three files, and they must agree:

| File | Why it matters |
|---|---|
| `app/src-tauri/Cargo.toml` | `env!("CARGO_PKG_VERSION")` — what `update.rs::current_version()` compares against the newest tag. Wrong here and the shipped app misreports its own version, or nags every user forever. |
| `app/src-tauri/tauri.conf.json` | The bundle version, and the DMG filename CI builds. |
| `app/src-tauri/Cargo.lock` | Carries `oatmeal-app`'s version. Refresh it, don't hand-edit. |

```sh
# after editing Cargo.toml and tauri.conf.json:
cd app/src-tauri && cargo check --lib   # rewrites Cargo.lock's version
```

Verify all three agree before committing:

```sh
grep -m1 '^version' app/src-tauri/Cargo.toml
grep -m1 '"version"' app/src-tauri/tauri.conf.json
grep -A1 'name = "oatmeal-app"' app/src-tauri/Cargo.lock | grep version
```

`package.json` at the repo root is unrelated (it has been `1.0.0` across every
release). Leave it alone.

## Step 4 — Commit

```sh
git add app/src-tauri/Cargo.toml app/src-tauri/tauri.conf.json app/src-tauri/Cargo.lock
git commit -m "Bump version to X.Y.Z"
git push
```

## Step 5 — Merge and tag

**A Claude Code session must not do this part.** Never push to `main`, never
merge into it — a hard operating boundary in this project even when instructed
otherwise. Claude's own permission classifier also blocks pushing release tags,
which has failed twice in a row mid-release; retrying does not help.

So: Claude stops here and hands the user these commands, filled in. The user runs
them from their own session (the `!` prefix in chat works).

```sh
# 1. Land the work on main (via the PR, or fast-forward if it is already merged)
git checkout main
git fetch origin
git merge --ff-only origin/main

# 2. Confirm main really carries the new version BEFORE tagging.
#    Check the CONTENT, not the commit subject: when the branch lands through a
#    PR, HEAD is "Merge pull request #N", so looking for the bump commit at the
#    tip proves nothing either way.
grep -m1 '"version"' app/src-tauri/tauri.conf.json   # must be the new version
grep -m1 '^version' app/src-tauri/Cargo.toml         # must match it

# 3. Tag it, verify the tag, then push the tag
git tag vX.Y.Z
git show vX.Y.Z:app/src-tauri/Cargo.toml | grep -m1 '^version'   # must be X.Y.Z
git push origin vX.Y.Z
```

**Tagging a commit that lacks the bump is the worst outcome on this list**, and
it fails quietly. CI names the DMG from `tauri.conf.json`, so you get
`Oatmeal-<old>-apple-silicon.dmg` attached to a release tagged `v<new>`. Worse,
the shipped app reports `CARGO_PKG_VERSION` as the old version while the newest
tag is the new one — so `update.rs` tells **every user, including someone who
just installed it,** that an update is available, forever, with no way to
satisfy the prompt. Check the file contents, not the log.

A concrete way this happens: two PRs are open from the same branch, an earlier
one gets merged, and the bump is still sitting in the later one. `main` looks
updated — the features are all there — and the version is silently still old.

## Step 6 — Optional: make the release mandatory

A release only blocks older builds if its notes contain this on a **line of its
own** (prose mentioning it does not count, deliberately):

```
Oatmeal-Minimum-Version: X.Y.Z
```

Leave it out and the new release is a dismissible suggestion. Only add it when an
older build is genuinely unsafe to keep using — for example when it would write
data in a shape the new build cannot read. Add it by editing the published
release notes after the build lands.

## Step 7 — Verify the release actually happened

```sh
gh run list --workflow=release.yml --limit 1
curl -fsSL -H "User-Agent: Oatmeal" \
  https://api.github.com/repos/Cujoqt/oatmeal/releases/latest \
  | grep -E '"tag_name"|browser_download_url'
```

The DMG must be attached — the update prompt sends people to it, and a release
with no asset is a dead end. Report what the API actually returned.

## Known, unfixed: the update is a rough landing

Not caused by shipping, but true of every release, and worth repeating to the
user rather than letting them discover it:

- The DMG is **ad-hoc signed and not notarized** (`signingIdentity: "-"`, and CI
  has no signing steps), so `spctl` rejects it and Gatekeeper blocks first launch.
- The release-notes template says "right-click and choose **Open**". Apple removed
  that bypass in Sequoia; on current macOS the user must go to
  **System Settings → Privacy & Security → Open Anyway**. The template is stale.
- Every release is a **new code identity**, so TCC forgets the grants and users
  re-authorise Microphone and Screen Recording after each update.

The real fix is a Developer ID signature plus notarization in CI.
