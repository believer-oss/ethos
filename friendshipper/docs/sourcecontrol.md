# Overview

Friendshipper is designed to be a one-stop-shop for all nontechnical users' source control needs. For Unreal projects, 
it essentially replaces workflows that UGS and P4V support. Features:
* Commit history
* Syncing repo + editor binaries
* Submitting files + optional commit message formatting
* Reverting files
* LFS lock management
* Generating code project files + opening editor
* Automatic and manual snapshots of worktree state

# Setup

You will need:
* Github repo
* Personal Access Token

If using the editor/engine binary download features, you will also need:
* [AWS creds and setup](https://github.com/believer-oss/ethos/blob/rjd/readme-updates/friendshipper/docs/setup.md#aws)

Upon first launch of Friendshipper, you will be prompted to fill in this information. You can always update it in the Preferences page later.

# Quick Submit

One of the main drawbacks of Git in a large team environment is the requirement to be on 
latest to submit changes. To overcome this limitation, Friendshipper submits changes through 
an alternate submission path that leverages Github's 
[Merge Queue](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue)
feature. This means that Friendshipper is tightly integrated with GitHub's feature set, but it
achieves parity with traditional Perforce workflows in the sense that users can submit files
at any given time, as long as it hasn't changed upstream and other users haven't locked it.

The way this works:
1. The user initiates a submit
2. A snapshot is saved in case anything goes wrong, the user won't lose any work
3. A new branch is created with all the changes
4. The changes are commited to the new branch
5. A temporary worktree branch is created, with all the same changes
6. Latest is fetched from upstream trunk and merged into the worktree branch
7. The worktree branch is pushed up to the remote
8. A new PR is opened to merge the change into the trunk branch
9. The PR is added to the merge queue
10. Once the PR passes all merge checks, it's merged into trunk

Because the Quick Submit process leaves the user on a leaf branch, Friendshipper uses the Sync
operation to switch the user back to the trunk branch if they're not on it already.

It should be noted that this process is mainly intended to be used by nontechnical users, or for
content-only submits. Engineers may want to have more control over their submission process and
are empowered to do so.

# [Unreal Plugin](https://github.com/believer-oss/FriendshipperSourceControl)

Unreal requires tight integration with whatever source control provider you're using for your 
repo, so Friendshipper has a plugin to facilitate this. See the plugin page for installation
documentation.
