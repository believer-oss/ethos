<!-- markdownlint-disable MD013 -->

# Overview

Friendshipper utilizes [longtail](https://github.com/DanEngelbrecht/golongtail) to store and manage build artifacts. Artifacts are stored in S3 after the build process is complete, and the resulting bucket listing is used to populate the versions available to launch, sync, or create a playtest.

## Build Pipeline

We're utilizing [Argo Workflows](https://argoproj.github.io/workflows/) to orchestrate the build jobs based on [push event webhooks](https://docs.github.com/en/webhooks/webhook-events-and-payloads#push) from GitHub. The CI system isn't critical, but we do populate the `Builds` page with the status of the kubernetes workflow custom resources. We previously used GitHub Actions with kubernetes self-hosted runners to perform a similar build and upload.

### Repository Setup

The GitHub repository is configured to require:

- PRs before merging into `main`
- [merge queue](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue)
- successful runs of a pre-merge test job on both Windows and Linux platforms
- linear history

### Argo Workflows Event Binding

On receiving the webhook, we evaluate:

- the `discriminator` field to identify game or engine build
- the branch, which determines if this is a merge-queue or main branch build
- the commit message, which can be used to trigger one-off builds

This calls into a workflows template, passing in:

- metadata for the build
- build template arguments
- repository
- revision

We currently have WorkflowEventBindings for these events:

- Engine builds (Windows and Linux)
- Editor build and test (Windows and Linux)
- Game client builds (Windows)
- Game server builds (Linux)

### Engine Builds

On engine builds, we run the [InstalledEngineBuild.xml](https://github.com/EpicGames/UnrealEngine/blob/ue5-main/Engine/Build/InstalledEngineBuild.xml) BuildGraph with a target of "Make Installed Build $Platform". The resulting `LocalBuilds/Engine/$Platform` directory is uploaded to S3 via longtail with roughly the following commands:

```bash
platform="linux"
configuration="development"
bucket="..."
git_repo_name="..."
git_sha="..."

soure_path=LocalBuilds/Engine/$platform
target_path="s3://$bucket/v1/$git_repo_name/engine/$platform/$configuration/$git_sha.json"
storage_path="s3://$bucket/v1/$git_repo_name/engine/$platform/data-$date_shard"

longtail \
  put \
  --source-path $source_path \
  --target-path $target_path \
  --storage-uri $storage_path \
  --exclude-filter-regex ".*\.debug$" \
  --show-stats --log-level=info --log-to-console

target_path="s3://$bucket/v1/$git_repo_name/engine-symbols/$platform/$configuration/$git_sha.json"
longtail \
  put \
  --source-path $source_path \
  --target-path $target_path \
  --storage-uri $storage_path \
  --include-filter-regex "(.*\.debug|/)$" \
  --show-stats --log-level=info --log-to-console
```

and the equivalent for Windows via powershell, using `(.*pdb|/)$` and `.*pdb$` as the regex filters.

### Game Builds

On game push events to branches that are not from the merge queue or main, we check the first line of the commit message for a tag to direct us to act like it's another build type, but otherwise ignore the push. When the developer is ready to merge the PR they click the PRs merge button, which will attempt to merge together PRs received in a short window of time, rebase them to a linear history, and fire a push event on the resulting branch. This triggers a template that ensures the compile is successful and that unit tests run on each platform. This run concludes by updating the GitHub status API, which allows the merge queue to proceed with the merge.

On push events to main, the Windows client build and Linux server build are kicked off. After building, the Windows client is uploaded to S3 via longtail, and the Linux server is uploaded to the ECR registry, where the kubernetes cluster can pull from.

The compile itself is done via a modified version of the [vela-games/circleci-ue5-game](https://github.com/vela-games/circleci-ue5-game/blob/main/Tools/BuildGraph.xml) BuildGraph.xml file. On the merge queue branch we run the call the [Stage](https://dev.epicgames.com/documentation/en-us/unreal-engine/buildgraph-script-tasks-reference-for-unreal-engine#stage) task on the editor target, and then separately run an in-house unit test runner. On the main branch we run the `Linux Pak and Stage` and `Windows Pak and Stage` targets, which perform the `Compile` and `BuildCookRun` tasks.

After building the server, a Docker image is created using a simple Dockerfile that copies the game binaries into the container and an `entrypoint.sh` to run the server. Our build container is based on the [ue4-docker](https://github.com/adamrehn/ue4-docker) build-prerequisites image, which creates a ue4 user at uid 1000, so we ensure the server is run as that user. [Kaniko](https://github.com/GoogleContainerTools/kaniko) is used to build the image and push it to the ECR registry, which is called with `--build-arg="UPROJECT=$uproject_name"` to set the correct path and binary name.

```Dockerfile
FROM debian:stable-slim
ARG USERNAME=ue4
ARG USER_UID=1000
ARG USER_GID=${USER_UID}
ARG DEBIAN_FRONTEND=noninteractive

ARG UPROJECT=TP_ThirdPerson
ENV UPROJECT=${UPROJECT}

# Create the user - Uncomment for sudo access inside container
#RUN groupadd --gid $USER_GID $USERNAME \
#  && useradd --uid $USER_UID --gid $USER_GID -m $USERNAME \
#  && apt-get update \
#  && apt-get install -y sudo \
#  && echo $USERNAME ALL=\(root\) NOPASSWD:ALL > /etc/sudoers.d/$USERNAME \
#  && chmod 0440 /etc/sudoers.d/$USERNAME \
#  && apt-get clean \
#  && rm -rf /var/lib/apt/lists/*

COPY --chown=ue4:ue4 . /app/
USER ${USERNAME}
EXPOSE 7777/udp
ENTRYPOINT ["/app/entrypoint.sh"]
```

```bash
#!/bin/sh

[ -z "${UPROJECT}" ] && echo "UPROJECT not set" && exit 1

echo "Running ${UPROJECT} in ${PWD}"
exec "/app/${UPROJECT}/Binaries/Linux/${UPROJECT}Server" "${@}"
```

After building the client, a longtail upload similar to the above is run, roughly:

```powershell
$platform = "win64"
$configuration = "development"
$bucket = "..."
$git_repo_name = "..."
$git_sha = "..."
$date_shard = "..."

$source_path = "stage/WindowsClient""
$target_path = "s3://$bucket/v1/$git_repo_name/client/$platform/$configuration/$git_sha.json"
$storage_path = "s3://$bucket/v1/$git_repo_name/client/$platform/data-$date_shard"

longtail `
  put `
  --source-path $source_path `
  --target-path $target_path `
  --storage-uri $storage_path `
  --exclude-filter-regex ".*\.pdb$" `
  --show-stats --log-level=info --log-to-console;

$target_path = "s3://$bucket/v1/$git_repo_name/client/$platform/$configuration/$git_sha.json"
longtail `
  put `
  --source-path $source_path `
  --target-path $target_path `
  --storage-uri $storage_path `
  --include-filter-regex "(.*pdb|/)$" `
  --show-stats --log-level=info --log-to-console;
```

## Longtail Directory Structure

This example shows two games and an Unreal Engine build, using the development configuration, with Linux and Windows versions. Separate longtail uploads are performed on the binaries and the symbols, but utilize the same `storage-uri` to allow for combined downloads. The `storage-uri` path is sharded, which creates duplicate blocks in S3 but prevents the store index from growing without bound. These artifacts are stored in S3, and are organized under the following hierarchy:

```text
v1/
├── believerco-$game1
│   ├── client
│   │   └── win64
│   │       ├── data-2024-08
│   │       └── development
│   │           └── version-data
│   │               ├── version-index
│   │               └── version-store-index
│   ├── client-symbols
│   │   └── win64
│   │       └── development
│   │           └── version-data
│   │               ├── version-index
│   │               └── version-store-index
│   ├── editor
│   │   ├── linux
│   │   │   ├── data-2024-08
│   │   │   └── development
│   │   │       └── version-data
│   │   │           ├── version-index
│   │   │           └── version-store-index
│   │   └── win64
│   │       ├── data-2024-08
│   │       └── development
│   │           └── version-data
│   │               ├── version-index
│   │               └── version-store-index
│   └── editor-symbols
│       ├── linux
│       │   └── development
│       │       └── version-data
│       │           ├── version-index
│       │           └── version-store-index
│       └── win64
│           └── development
│               └── version-data
│                   ├── version-index
│                   └── version-store-index
├── believerco-$game2
│   ├── client
│   │   └── win64
│   │       ├── data-2024-08
│   │       └── development
│   │           └── version-data
│   │               ├── version-index
│   │               └── version-store-index
│   ├── client-symbols
│   │   └── win64
│   │       └── development
│   │           └── version-data
│   │               ├── version-index
│   │               └── version-store-index
│   ├── editor
│   │   ├── linux
│   │   │   ├── data-2024-08
│   │   │   └── development
│   │   │       └── version-data
│   │   │           ├── version-index
│   │   │           └── version-store-index
│   │   └── win64
│   │       ├── data-2024-08
│   │       └── development
│   │           └── version-data
│   │               ├── version-index
│   │               └── version-store-index
│   └── editor-symbols
│       ├── linux
│       │   └── development
│       │       └── version-data
│       │           ├── version-index
│       │           └── version-store-index
│       └── win64
│           └── development
│               └── version-data
│                   ├── version-index
│                   └── version-store-index
└── believerco-unrealengine
    ├── engine
    │   ├── linux
    │   │   ├── data-2024-08
    │   │   └── development
    │   │       └── version-data
    │   │           ├── version-index
    │   │           └── version-store-index
    │   └── win64
    │       ├── data-2024-08
    │       └── development
    │           └── version-data
    │               ├── version-index
    │               └── version-store-index
    └── engine-symbols
        ├── linux
        │   └── development
        │       └── version-data
        │           ├── version-index
        │           └── version-store-index
        └── win64
            └── development
                └── version-data
                    ├── version-index
                    └── version-store-index
```
