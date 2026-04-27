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

## License

This project is licensed under the MIT License - see the [LICENSE](./LICENSE) file for details.

## Acknowledgements

- Martin Sturm (@sturmm) for granting us permission to use [aws-easy-sso](https://github.com/sturmm/aws-easy-sso)'s auth code for AWS SSO integration.