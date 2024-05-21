<script lang="ts">
	import { Accordion, AccordionItem, Button, Card, Spinner, Tooltip } from 'flowbite-svelte';
	import { FileCopyOutline, RefreshOutline } from 'flowbite-svelte-icons';
	import { json } from 'svelte-highlight/languages';
	import Highlight from 'svelte-highlight';
	import 'svelte-highlight/styles/github-dark.css';
	import { emit } from '@tauri-apps/api/event';
	import { onMount } from 'svelte';
	import { repoStatus } from '$lib/stores';
	import { fixRebase, getRebaseStatus, getRepoStatus, rebase } from '$lib/repo';
	import { getUnrealVersionSelectorStatus } from '$lib/system';
	import { CheckStatus, type RebaseStatusResponse } from '$lib/types';
	import EmojiStatus from '$lib/components/EmojiStatus.svelte';

	// Various check statuses
	let repoStatusCheck: CheckStatus = CheckStatus.Loading;
	let mergeConflictCheck: CheckStatus = CheckStatus.Loading;
	let rebaseCheck: CheckStatus = CheckStatus.Loading;
	let rebaseRequiredCheck: CheckStatus = CheckStatus.Loading;
	let unrealVersionSelectorCheck: CheckStatus = CheckStatus.Loading;

	let loading = false;
	let updatingRebaseStatus = false;
	let rebasing = false;

	let rebaseStatus: RebaseStatusResponse = {
		rebaseMergeExists: false,
		headNameExists: false
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
		unrealVersionSelectorCheck = CheckStatus.Loading;

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
	</Accordion>
</Card>
