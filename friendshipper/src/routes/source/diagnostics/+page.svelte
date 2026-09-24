<script lang="ts">
	import { Accordion, AccordionItem, Button, Card, Modal, Spinner, Tooltip } from 'flowbite-svelte';
	import { FileCopyOutline, RefreshOutline } from 'flowbite-svelte-icons';
	import { json } from 'svelte-highlight/languages';
	import Highlight from 'svelte-highlight';
	import 'svelte-highlight/styles/github-dark.css';
	import { emit } from '@tauri-apps/api/event';
	import { onMount } from 'svelte';
	import { repoStatus } from '$lib/stores';
	import {
		fixRebase,
		getGithubStatus,
		getObjectCount,
		getRebaseStatus,
		getRepoStatus,
		rebase,
		runGitGc,
		forceDownloadDlls,
		forceDownloadEngine,
		getArtifactStatus,
		verifyArtifact
	} from '$lib/repo';
	import { getUnrealVersionSelectorStatus } from '$lib/system';
	import {
		CheckStatus,
		type GitHubStatusResponse,
		type ObjectCountResponse,
		type RebaseStatusResponse,
		type ArtifactStatus,
		type SyncKind,
		type VerifyResponse
	} from '$lib/types';
	import EmojiStatus from '$lib/components/EmojiStatus.svelte';

	// Various check statuses
	let repoStatusCheck: CheckStatus = CheckStatus.Loading;
	let mergeConflictCheck: CheckStatus = CheckStatus.Loading;
	let rebaseCheck: CheckStatus = CheckStatus.Loading;
	let rebaseRequiredCheck: CheckStatus = CheckStatus.Loading;
	let objectCountCheck: CheckStatus = CheckStatus.Loading;
	let unrealVersionSelectorCheck: CheckStatus = CheckStatus.Loading;
	let githubStatusCheck: CheckStatus = CheckStatus.Loading;
	let githubStatus: GitHubStatusResponse | null = null;

	let loading = false;
	let updatingRebaseStatus = false;
	let rebasing = false;
	let runningGc = false;

	// Two separate questions, deliberately. "Is the right version installed?" is answered
	// by the checkout - the uproject for the engine, your commit for the editor binaries -
	// and is fixed by syncing. "Are its bytes intact?" is answered by verifying, which
	// repairs in place and never removes anything. Verifying across a version change would
	// merge the two builds rather than replace one, so it refuses.
	const verifyLabels: Record<SyncKind, string> = {
		client: 'Game client',
		engine: 'Engine',
		editorDlls: 'Editor binaries'
	};
	let artifactStatuses: ArtifactStatus[] = [];
	let verifying: SyncKind | null = null;
	let syncing: SyncKind | null = null;
	let verifyResults: Partial<Record<SyncKind, VerifyResponse>> = {};
	let artifactCheck: CheckStatus = CheckStatus.Loading;

	/**
	 * One icon for three artifacts, so the collapsed row says whether it is worth opening.
	 *
	 * Only "installed, but not the version this checkout needs" is a red mark: it is the
	 * one state with something to do about it. Never having synced is a starting point,
	 * not a fault, and the game client has no version to be wrong about. "Unknown" gets
	 * the shrug rather than going green or sitting on the thinking face - we could not
	 * work out what should be installed, which will not resolve on its own, and claiming
	 * a clean bill of health we never established would be worse than saying so.
	 */
	const summarise = (statuses: ArtifactStatus[]): CheckStatus => {
		if (statuses.length === 0) return CheckStatus.Loading;
		if (statuses.some((s) => s.state === 'outOfDate')) return CheckStatus.Failure;
		if (statuses.some((s) => s.state === 'unknown')) return CheckStatus.Unknown;
		return CheckStatus.Success;
	};

	const artifactHints: Record<CheckStatus, string> = {
		[CheckStatus.Loading]: 'Checking what is installed',
		[CheckStatus.Success]: 'Everything installed matches this checkout',
		[CheckStatus.Failure]:
			'Something installed is not the version this checkout needs - open for details',
		[CheckStatus.Unknown]: 'Could not work out which version this checkout needs - open for details'
	};

	const refreshArtifacts = async () => {
		try {
			artifactStatuses = await getArtifactStatus();
			artifactCheck = summarise(artifactStatuses);
		} catch (e) {
			artifactCheck = CheckStatus.Failure;
			await emit('error', e);
		}
	};

	const handleVerify = async (kind: SyncKind) => {
		verifying = kind;
		try {
			verifyResults[kind] = await verifyArtifact(kind);
			verifyResults = verifyResults;
			await refreshArtifacts();
		} catch (e) {
			await emit('error', e);
		} finally {
			verifying = null;
		}
	};

	// Reuses the normal sync, so it queues on the repo worker and reports on the status
	// bar exactly as it would if you had triggered it any other way.
	const handleSync = async (status: ArtifactStatus) => {
		const { kind } = status;
		syncing = kind;
		try {
			if (kind === 'engine') {
				await forceDownloadEngine();
			} else if (kind === 'editorDlls') {
				// The build this row says it needs, not whatever a pull would fetch. Those
				// differ whenever the checkout is behind origin, and fetching the other one
				// installs binaries from ahead of the checkout - which this page then
				// reports as out of date all over again, with the button that caused it.
				await forceDownloadDlls(status.expected ?? undefined);
			}
			// A sync replaces the artifact, so whatever the last verify said is now stale.
			verifyResults = Object.fromEntries(
				Object.entries(verifyResults).filter(([existing]) => existing !== kind)
			);
			await refreshArtifacts();
		} catch (e) {
			await emit('error', e);
		} finally {
			syncing = null;
		}
	};

	let rebaseStatus: RebaseStatusResponse = {
		rebaseMergeExists: false,
		headNameExists: false
	};

	let objectCountStatus: ObjectCountResponse = {
		inPackCount: 0,
		isHealthy: true,
		rawOutput: '',
		looseCount: 0,
		lastPacked: null
	};

	let unrealVersionSelectorStatus = {
		valid_version_selector: false,
		version_selector_msg: '',
		uproject_file_assoc: false,
		uproject_file_assoc_msg: ['']
	};

	const refresh = async () => {
		loading = true;
		updatingRebaseStatus = true;

		repoStatusCheck = CheckStatus.Loading;
		mergeConflictCheck = CheckStatus.Loading;
		rebaseCheck = CheckStatus.Loading;
		rebaseRequiredCheck = CheckStatus.Loading;
		objectCountCheck = CheckStatus.Loading;
		unrealVersionSelectorCheck = CheckStatus.Loading;
		githubStatusCheck = CheckStatus.Loading;

		try {
			repoStatus.set(await getRepoStatus());

			repoStatusCheck = CheckStatus.Success;

			if ($repoStatus?.conflictUpstream) {
				mergeConflictCheck = CheckStatus.Failure;
			} else {
				mergeConflictCheck = CheckStatus.Success;
			}

			if (
				$repoStatus?.commitsAhead &&
				$repoStatus?.commitsBehind &&
				$repoStatus?.commitsBehind > 0 &&
				$repoStatus?.commitsAhead > 0
			) {
				rebaseRequiredCheck = CheckStatus.Failure;
			} else {
				rebaseRequiredCheck = CheckStatus.Success;
			}
		} catch (e) {
			await emit('error', e);

			repoStatusCheck = CheckStatus.Failure;
			mergeConflictCheck = CheckStatus.Failure;
		}

		try {
			rebaseStatus = await getRebaseStatus();
			if (rebaseStatus.rebaseMergeExists || rebaseStatus.headNameExists) {
				rebaseCheck = CheckStatus.Failure;
			} else {
				rebaseCheck = CheckStatus.Success;
			}
		} catch (e) {
			await emit('error', e);
			rebaseCheck = CheckStatus.Failure;
		}

		try {
			objectCountStatus = await getObjectCount();
			if (objectCountStatus.isHealthy) {
				objectCountCheck = CheckStatus.Success;
			} else {
				objectCountCheck = CheckStatus.Failure;
			}
		} catch (e) {
			await emit('error', e);
			objectCountCheck = CheckStatus.Failure;
		}

		try {
			unrealVersionSelectorStatus = await getUnrealVersionSelectorStatus();
			if (
				unrealVersionSelectorStatus.valid_version_selector ||
				unrealVersionSelectorStatus.uproject_file_assoc
			) {
				unrealVersionSelectorCheck = CheckStatus.Success;
			} else {
				unrealVersionSelectorCheck = CheckStatus.Failure;
			}
		} catch (e) {
			await emit('error', e);
			unrealVersionSelectorCheck = CheckStatus.Failure;
		}

		try {
			githubStatus = await getGithubStatus();
			githubStatusCheck =
				githubStatus.indicator === 'none' ? CheckStatus.Success : CheckStatus.Failure;
		} catch (e) {
			await emit('error', e);
			githubStatus = null;
			githubStatusCheck = CheckStatus.Failure;
		}

		updatingRebaseStatus = false;
		loading = false;
	};

	const handleFixRebase = async () => {
		updatingRebaseStatus = true;
		try {
			await fixRebase();
			await emit('success', 'Rebase fixed!');
		} catch (e) {
			await emit('error', e);
		} finally {
			await refresh();
			updatingRebaseStatus = false;
		}
	};

	const handleRunGc = async () => {
		runningGc = true;
		try {
			await runGitGc();
			await emit('success', 'git gc complete!');
		} catch (e) {
			await emit('error', e);
		} finally {
			await refresh();
			runningGc = false;
		}
	};

	const handleRebase = async () => {
		rebasing = true;
		try {
			await rebase();
			await emit('success', 'Rebase successful!');
		} catch (e) {
			await emit('error', e);
		} finally {
			await refresh();
			rebasing = false;
		}
	};

	onMount(() => {
		void refresh();
		void refreshArtifacts();
	});
</script>

<div class="flex items-center justify-between gap-2">
	<div class="flex items-center gap-2">
		<p class="text-2xl my-2 text-primary-400 dark:text-primary-400">Repo Diagnostics</p>
		<Button disabled={loading} class="!p-1.5" primary on:click={refresh}>
			{#if loading}
				<Spinner size="4" />
			{:else}
				<RefreshOutline class="w-4 h-4" />
			{/if}
		</Button>
		<Button
			disabled={loading}
			class="!p-1.5"
			on:click={() => navigator.clipboard.writeText(JSON.stringify($repoStatus, null, 2))}
		>
			<FileCopyOutline class="w-4 h-4" />
		</Button>
		<Tooltip
			class="w-auto text-xs text-primary-400 bg-secondary-700 dark:bg-space-900"
			placement="bottom"
			>Copy diagnostic data to clipboard
		</Tooltip>
	</div>
</div>
<Card
	class="w-full p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 max-h-screen overflow-auto border-0 shadow-none"
>
	<Accordion
		activeClass="hover:bg-secondary-800 dark:hover:bg-space-950 focus:ring-0 text-white overflow-auto py-2"
		inactiveClass="hover:bg-secondary-800 dark:hover:bg-space-950 text-white py-2"
		class="w-full"
	>
		<AccordionItem class="w-full">
			<div slot="header" class="flex items-center justify-between w-full pr-2">
				<div class="w-1/3">Repo Status Data</div>
				<span class="text-xs text-gray-300 font-mono w-3/4"
					>Can we successfully get Git status from the Friendshipper backend?</span
				>
				<EmojiStatus checkStatus={repoStatusCheck} />
			</div>
			<Highlight
				class="text-xs font-mono tracking-wider"
				language={json}
				code={JSON.stringify($repoStatus, null, 2)}
			/>
		</AccordionItem>
		<AccordionItem class="w-full">
			<div slot="header" class="flex items-center justify-between w-full pr-2">
				<div class="w-1/3">Merge Conflict Status</div>
				<span class="text-xs text-gray-300 font-mono w-3/4"
					>Do we have local file changes that conflict with upstream changes?</span
				>
				<EmojiStatus checkStatus={mergeConflictCheck} />
			</div>
			{#if $repoStatus?.conflictUpstream}
				<Highlight
					class="text-xs font-mono tracking-wider"
					language={json}
					code={JSON.stringify($repoStatus?.conflicts, null, 2)}
				/>
			{:else}
				No merge conflicts!
			{/if}
		</AccordionItem>
		<AccordionItem class="w-full">
			<div slot="header" class="flex items-center justify-between w-full pr-2">
				<div class="w-1/3">Rebase Status</div>
				<span class="text-xs text-gray-300 font-mono w-3/4"
					>Are we stuck in the middle of a sync operation?</span
				>
				<EmojiStatus checkStatus={rebaseCheck} />
			</div>
			{#if rebaseStatus.headNameExists || rebaseStatus.rebaseMergeExists}
				<div class="flex items-center gap-2">
					<span
						>Rebase detected. You can attempt to fix this by clicking the button to the right.</span
					>
					<Button disabled={updatingRebaseStatus} size="sm" primary on:click={handleFixRebase}>
						{#if updatingRebaseStatus}
							<Spinner size="4" />
						{:else}
							Auto-fix
						{/if}
					</Button>
				</div>
			{:else}
				No rebase detected!
			{/if}
		</AccordionItem>
		<AccordionItem class="w-full">
			<div slot="header" class="flex items-center justify-between w-full pr-2">
				<div class="w-1/3">Additional Rebase Needed?</div>
				<span class="text-xs text-gray-300 font-mono w-3/4">Do we need to rebase on upstream?</span>
				<EmojiStatus checkStatus={rebaseRequiredCheck} />
			</div>
			{#if rebaseRequiredCheck === CheckStatus.Failure}
				<div class="flex items-center gap-2">
					<span
						>Local repo is {$repoStatus?.commitsAhead} commit(s) ahead and {$repoStatus?.commitsBehind}
						commit(s) behind. Let's try a rebase!</span
					>
					<Button disabled={updatingRebaseStatus} size="sm" primary on:click={handleRebase}>
						{#if rebasing}
							<Spinner size="4" />
						{:else}
							Rebase
						{/if}
					</Button>
				</div>
			{:else}
				No rebase required!
			{/if}
		</AccordionItem>
		<AccordionItem class="w-full">
			<div slot="header" class="flex items-center justify-between w-full pr-2">
				<div class="w-1/3">Git Object Count</div>
				<span class="text-xs text-gray-300 font-mono w-3/4"
					>Is the Git object store healthy? (in-pack &lt; 25M objects)</span
				>
				<EmojiStatus checkStatus={objectCountCheck} />
			</div>
			{#if objectCountCheck === CheckStatus.Failure}
				<div class="flex flex-col gap-2">
					<span
						>Warning: Git object store has {objectCountStatus.inPackCount.toLocaleString()} objects in
						packfiles, which exceeds the 25 million object threshold. Consider running git gc to optimize
						the repository.</span
					>
					<Highlight
						class="text-xs font-mono tracking-wider"
						language={json}
						code={objectCountStatus.rawOutput}
					/>
				</div>
			{:else}
				<div class="flex flex-col gap-2">
					<span
						>Git object store looks healthy! Objects in packfiles: {objectCountStatus.inPackCount.toLocaleString()}</span
					>
					<Highlight
						class="text-xs font-mono tracking-wider"
						language={json}
						code={objectCountStatus.rawOutput}
					/>
				</div>
			{/if}
			{#if objectCountCheck !== CheckStatus.Loading}
				<div class="flex items-center gap-2 mt-2">
					<Button disabled={runningGc || loading} size="sm" primary on:click={handleRunGc}>
						Run git gc
					</Button>
					<span class="text-xs text-gray-300"
						>Runs <code>git gc</code> to optimize the repository. Unreachable objects newer than two
						weeks are kept so lost commits can still be recovered. This may take a while. For a fuller
						pass that also expires old reflog entries and rebuilds the commit-graph, see Background Operations
						in Preferences.</span
					>
				</div>
			{/if}
		</AccordionItem>
		<AccordionItem class="w-full">
			<div slot="header" class="flex items-center justify-between w-full pr-2">
				<div class="w-1/3">Downloaded Artifacts</div>
				<span class="text-xs text-gray-300 font-mono w-3/4">
					Check an install against the build it came from
				</span>
				<EmojiStatus checkStatus={artifactCheck} hint={artifactHints[artifactCheck]} />
			</div>
			<div class="flex flex-col gap-3">
				<span class="text-xs text-gray-300">
					Syncing makes an artifact exactly the version this checkout needs, removing anything the
					new version does not contain. Verifying leaves the version alone and repairs files whose
					contents do not match - it never deletes, so files the build does not know about, like
					save games and local changes, are safe.
				</span>
				{#each artifactStatuses as status (status.kind)}
					<div class="flex flex-col gap-1 border-t border-secondary-800 dark:border-space-950 pt-2">
						<div class="flex items-center gap-2 flex-wrap">
							<span class="text-sm text-primary-300 w-32">{verifyLabels[status.kind]}</span>

							{#if status.state === 'notInstalled'}
								<span class="text-xs text-gray-400">Not installed</span>
							{:else}
								<span class="text-xs text-gray-300 font-mono">
									on disk: {status.installed}
								</span>
								{#if status.expected}
									<span
										class="text-xs font-mono {status.state === 'outOfDate'
											? 'text-yellow-300'
											: 'text-gray-300'}"
									>
										needs: {status.expected}
									</span>
								{/if}
								{#if status.state === 'installed'}
									<span class="text-xs text-green-400">up to date</span>
								{:else if status.state === 'outOfDate'}
									<span class="text-xs text-yellow-300">out of date</span>
								{:else if status.state === 'unknown'}
									<span class="text-xs text-gray-400">
										cannot tell which version this checkout needs
									</span>
								{/if}
							{/if}

							{#if status.state === 'outOfDate' && status.kind !== 'client'}
								<Button
									disabled={syncing !== null || verifying !== null}
									size="sm"
									primary
									on:click={() => handleSync(status)}
								>
									{#if syncing === status.kind}
										<Spinner size="4" />
									{:else}
										Sync
									{/if}
								</Button>
							{:else if status.state !== 'notInstalled' && status.state !== 'unknown'}
								<Button
									disabled={syncing !== null || verifying !== null}
									size="sm"
									primary
									on:click={() => handleVerify(status.kind)}
								>
									{#if verifying === status.kind}
										<Spinner size="4" />
									{:else}
										Verify
									{/if}
								</Button>
							{/if}
						</div>

						{#if status.expectationSource}
							<span class="text-xs text-gray-500 pl-1">
								Expected version comes from the {status.expectationSource}.
							</span>
						{/if}
						{#if status.location}
							<span class="text-xs text-gray-500 font-mono pl-1 truncate">{status.location}</span>
						{/if}
						{#if verifyResults[status.kind]}
							<span
								class="text-xs pl-1 {verifyResults[status.kind]?.checked &&
								verifyResults[status.kind]?.repaired === 0
									? 'text-green-400'
									: 'text-yellow-300'}"
							>
								{verifyResults[status.kind]?.message}
							</span>
						{/if}
					</div>
				{/each}
			</div>
		</AccordionItem>
		<AccordionItem class="w-full">
			<div slot="header" class="flex items-center justify-between w-full pr-2">
				<div class="w-1/3">Unreal Version Selector?</div>
				<span class="text-xs text-gray-300 font-mono w-3/4"
					>Is the Unreal Version Selector installed and configured?</span
				>
				<EmojiStatus checkStatus={unrealVersionSelectorCheck} />
			</div>
			{#if unrealVersionSelectorCheck === CheckStatus.Failure}
				<div class="flex items-center gap-2">
					<span />
				</div>
				<Highlight
					class="text-xs font-mono tracking-wider"
					language={json}
					code={JSON.stringify(unrealVersionSelectorStatus, null, 2)}
				/>
			{:else}
				Everything looks good!
			{/if}
		</AccordionItem>
		<AccordionItem class="w-full">
			<div slot="header" class="flex items-center justify-between w-full pr-2">
				<div class="w-1/3">GitHub Status</div>
				<span class="text-xs text-gray-300 font-mono w-3/4"
					>Is GitHub reporting all systems operational?</span
				>
				<EmojiStatus checkStatus={githubStatusCheck} />
			</div>
			{#if githubStatus}
				<div class="flex flex-col gap-1">
					<span>
						<span class="font-semibold">{githubStatus.description}</span>
						<span class="text-xs text-gray-300">(indicator: {githubStatus.indicator})</span>
					</span>
					<a
						class="text-xs text-primary-400 underline"
						href={githubStatus.url}
						target="_blank"
						rel="noopener noreferrer">{githubStatus.url}</a
					>
				</div>
			{:else}
				Could not reach the GitHub status page.
			{/if}
		</AccordionItem>
	</Accordion>
</Card>

<Modal
	defaultClass="bg-secondary-700 dark:bg-space-900 overflow-y-auto"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	dismissable={false}
	size="sm"
	open={runningGc}
>
	<div class="flex items-center gap-3">
		<Spinner size="6" />
		<div class="flex flex-col">
			<p class="text-xl text-primary-400">Running git gc</p>
			<p class="text-sm text-gray-300">
				Please don't close Friendshipper or run other git operations. This may take a while.
			</p>
		</div>
	</div>
</Modal>
