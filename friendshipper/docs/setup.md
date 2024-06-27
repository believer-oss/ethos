# Friendshipper Setup Requirements

If you're reading this, you're probably interested in setting up Friendshipper for your own game project. This document will outline the constraints for doing so.

## High-Level Technical Couplings

Friendshipper currently relies on:

- AWS
  - S3 for storing game client/server builds
  - EKS for running game servers and managing playtests
    - [f11r-operator](https://github.com/believer-os/f11r-operator) is a Kubernetes operator that stores GameServers and Playtests as custom resources. Friendshipper creates these resources directly against the Kubernetes API.
  - SSO is the _only_ authentication method supported by Friendshipper currently
- Unreal Engine 5
  - Friendshipper provdes a mechanism for downloading new versions of your studio's source-built engine, but these mechanisms assume the project's engine is Unreal.
  - Friendshipper launches game servers with command line arguments that assume an Unreal Engine format.
- GitHub
  - Friendshipper uses GitHub's merge queue feature for submitting work. Currently, your repo must have merge queue enabled.
- Argo Workflows
  - Friendshipper uses Argo Workflows for running CI/CD pipelines. Friendshipper assumes that Argo Workflows is installed in the same Kubernetes cluster as the playtest game servers.

Ideally, Friendshipper would be more flexible in these areas. We should support different S3-like backends, different game engines, and different CI/CD providers, etc. Over time, we expect pluggable interfaces for these to develop, but of course we're very open to contributions. Additionally, we should allow for disabling certain features of Friendshipper, such as the merge queue, if you don't want to use them.

## Configuration Assumptions

Friendshipper assumes particular configuration settings across your infrastructure stack. We'll outline them here.

### AWS

WIP

#### SSO

WIP
