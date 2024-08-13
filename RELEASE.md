# Release Process

This document describes the process for releasing a new version of the `actors-utils` project.

## Current State

1. On a release pull request creation, a [Release Checker](.github/workflows/release-check.yml) workflow will run. It will perform the following actions:
    1. Extract the version from the modified `Cargo.toml` files. Process each crate in the workspace **independently**.
    2. Check if a git tag for the version, using the `crate_name@version` as the pattern, already exists. Continue only if it does not.
    3. Create a draft GitHub release with the version as the tag.
    4. Comment on the pull request with a link to the draft release.
2. **[MANUAL]** Run `cargo publish --dry-run` for each crate that is proposed to be released in the reverse dependency order.
3. On pull request merge, a [Releaser](.github/workflows/release.yml) workflow will run. It will perform the following actions:
    1. Extract the version from the modified `Cargo.toml` files. Process each crate in the workspace **independently**.
    2. Check if a git tag for the version, using the `crate_name@version` as the pattern, already exists. Continue only if it does not.
    3. Check if a draft GitHub release with the version as the tag exists.
    4. If the draft release exists, publish it. Otherwise, create a new release with the version as the tag.
4. **[MANUAL]** Run `cargo publish` for each crate that has been released in the reverse dependency order.

#### Known Limitations

1. `cargo publish --dry-run` has to be run manually.
2. `cargo publish` has to be run manually.

#### Possible Improvements

1. Run `cargo publish --dry-run` in the [reverse dependency order](#crate-dependency-graph) automatically. Use a local registry to simulate the dependencies that are not yet published.
2. Run `cargo publish` in the [**reverse dependency order**](#crate-dependency-graph) automatically after the merge.
