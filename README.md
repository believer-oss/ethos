<div align='center'>

# Ethos

**A collection of game development tools for working with Git**

[![Discord](https://img.shields.io/discord/1194345901687316520?logo=discord&logoColor=white&color=%235865F2)](https://discord.gg/pzEgMhynzP)
[![Build status](https://github.com/believer-oss/ethos/actions/workflows/rust.yml/badge.svg?branch=main)](https://github.com/believer-oss/ethos/actions/workflows/rust.yml)
</div>

### The stack

The tools here are desktop applications that share a tech stack:
- [Tauri](https://tauri.app/) as an application framework (Rust)
- [Svelte](https://svelte.dev/) + [SvelteKit](https://kit.svelte.dev/) for UI (Typescript)
- [Flowbite Svelte](https://flowbite-svelte.com/) for UI components (Typescript)

There's also an `ethos-core` crate which holds code that's shared between tools.

## What's in this repo?

Each tool has its own documentation, but here's a brief overview of each of them:

### Friendshipper

Friendshipper is a desktop application for viewing and downloading builds of an Unreal project. It can be used in one of two modes:

- **Playtest mode** - This mode is for everyone. Launch and connect to game servers at any version of your project, and Friendshipper will download the right client. Schedule, view, and join playtests in groups.
- **Contributor mode** - This mode is for engineers and content creators who work in the Unreal project's Git repo. This mode enables them to sync changes, submit changes, and view file locks in Git LFS.

All of our playtesting at Believer is done through Friendshipper. See full docs [here](./friendshipper/README.md).

### Birdie

Birdie is an application for art teams to manage their assets through Git LFS. At current state, it functions like a simple file browser, allowing users to browse, download, check out, and submit files via Git. Birdie utilizes specific Git configuration parameters to ensure that users can download and work on exactly the files they need without requiring them to sync all assets in the repository.

Birdie is in its early stages. Our plan for Birdie is to introduce a more well-defined game-centric taxonomy to files via metadata, enabling users to search for files by "character", "location", or "chapter", for example.

See Birdie's documentation [here](./birdie/README.md).

## License

This project is licensed under the MIT License - see the [LICENSE](./LICENSE) file for details.

## Acknowledgements

- Martin Sturm (@sturmm) for granting us permission to use [aws-easy-sso](https://github.com/sturmm/aws-easy-sso)'s auth code for AWS SSO integration.