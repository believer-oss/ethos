<script lang="ts">
	import { Button, Checkbox, Label, Modal, Spinner } from 'flowbite-svelte';
	import {
		CheckCircleSolid,
		ExclamationCircleSolid,
		QuestionCircleSolid
	} from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import { createEventDispatcher } from 'svelte';
	import { cleanupBranches, getBranchCleanupCandidates } from '$lib/repo';
	import {
		cleanupNote,
		cleanupResultMessages,
		cleanupSafeViaPr,
		localBranchCount,
		shortSha
	} from '$lib/branches';
	import type {
		BranchCleanupCandidate,
		BranchCleanupCandidates,
		BranchCleanupResult,
		Nullable,
		TauriError
	} from '$lib/types';

	export let open: boolean;
	export let busy = false;

	const dispatch = createEventDispatcher<{ cleaned: BranchCleanupResult[] }>();

	let data: Nullable<BranchCleanupCandidates> = null;
	let loading = false;
	let loadError: Nullable<string> = null;
	let deleting = false;
	let requestToken = 0;
	let wasOpen = false;
	// Keys are `name@sha`, so a branch whose tip moved is never selected by an old choice.
	let selected = new Set<string>();
	let seen = new Set<string>();
	// The exact branches the confirmation step deletes; null shows the list.
	let confirming: Nullable<BranchCleanupCandidate[]> = null;
	let acknowledged = false;

	const key = (c: BranchCleanupCandidate): string => `${c.name}@${c.sha}`;

	const errorMessage = (e: unknown): string => {
		const err = e as TauriError;
		return err?.message ? err.message : String(e);
	};

	const load = async () => {
		requestToken += 1;
		const token = requestToken;
		loading = true;
		loadError = null;
		try {
			const result = await getBranchCleanupCandidates();
			if (token !== requestToken) return;
			const keys = new Set(result.candidates.map(key));
			// Keep choices for branches still listed; pre-check only safe branches seen for the first time.
			const next = new Set([...selected].filter((k) => keys.has(k)));
			result.candidates.forEach((c) => {
				if (c.safe && !seen.has(key(c))) next.add(key(c));
			});
			seen = new Set([...seen, ...keys]);
			selected = next;
			data = result;
		} catch (e) {
			if (token !== requestToken) return;
			data = null;
			loadError = errorMessage(e);
		} finally {
			if (token === requestToken) loading = false;
		}
	};

	$: if (open !== wasOpen) {
		wasOpen = open;
		if (open) {
			data = null;
			loadError = null;
			selected = new Set();
			seen = new Set();
			confirming = null;
			acknowledged = false;
			void load();
		} else {
			requestToken += 1;
			loading = false;
		}
	}

	$: candidates = data?.candidates ?? [];
	$: trunk = data?.trunkBranch ?? '';
	$: selectedCandidates = candidates.filter((c) => selected.has(key(c)));
	// "Clean up all merged" never includes a branch that needs the acknowledgement.
	$: mergedCandidates = candidates.filter((c) => c.safe);
	$: locked = busy || loading || deleting;
	$: needsAck = (confirming ?? []).some((c) => !c.safe);
	$: canDelete =
		confirming !== null && confirming.length > 0 && !locked && (!needsAck || acknowledged);

	const toggle = (c: BranchCleanupCandidate, checked: boolean) => {
		const next = new Set(selected);
		if (checked) {
			next.add(key(c));
		} else {
			next.delete(key(c));
		}
		selected = next;
	};

	const onToggle = (c: BranchCleanupCandidate) => (e: Event) => {
		toggle(c, (e.target as HTMLInputElement).checked);
	};

	const review = (list: BranchCleanupCandidate[]) => {
		if (list.length === 0 || locked) return;
		confirming = list;
		acknowledged = false;
	};

	const reviewOne = (c: BranchCleanupCandidate) => () => {
		review([c]);
	};

	const reviewSelected = () => {
		review(selectedCandidates);
	};

	const reviewAllMerged = () => {
		review(mergedCandidates);
	};

	const back = () => {
		confirming = null;
		acknowledged = false;
	};

	const close = () => {
		if (deleting) return;
		open = false;
	};

	const runDelete = async () => {
		if (!confirming || !canDelete) return;
		const branches = confirming.map((c) => ({
			name: c.name,
			sha: c.sha,
			safe: c.safe,
			safeViaPr: cleanupSafeViaPr(c)
		}));
		deleting = true;
		try {
			const results = await cleanupBranches({ branches });
			const { success, failure } = cleanupResultMessages(results);
			if (success) await emit('success', success);
			if (failure) await emit('error', { message: failure });
			dispatch('cleaned', results);
		} catch (e) {
			await emit('error', e);
		} finally {
			deleting = false;
			back();
			if (open) void load();
		}
	};

	const toneClass = (tone: string): string => {
		if (tone === 'safe') return 'text-lime-400';
		if (tone === 'unknown') return 'text-gray-300';
		return 'text-yellow-300';
	};

	const dateText = (date: string): string => {
		const parsed = new Date(date);
		return Number.isNaN(parsed.getTime()) ? date : parsed.toLocaleString();
	};
</script>

<Modal
	defaultClass="bg-secondary-700 dark:bg-space-900 overflow-y-auto"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	size="lg"
	dismissable={!deleting}
	{open}
	on:close={close}
>
	{#if confirming === null}
		<div class="flex flex-row items-center gap-2">
			<p class="text-xl text-primary-400">Clean up deleted branches</p>
			{#if loading}
				<Spinner size="4" />
			{/if}
		</div>
		<p class="text-sm text-gray-300">
			Local <code>f11r-branch-*</code> branches whose branch on origin was deleted. Only your local copies
			are removed; remote branches are not affected.
		</p>
		{#if loadError}
			<p class="text-sm text-red-400 break-words">{loadError}</p>
		{:else if !data}
			<div class="flex justify-center p-2"><Spinner size="6" /></div>
		{:else if candidates.length === 0}
			<p class="text-sm text-gray-300">No local f11r-branch branches were deleted on origin.</p>
		{:else}
			{#if data.prError}
				<p class="text-xs text-gray-400 break-words">GitHub unavailable: {data.prError}</p>
			{/if}
			<div
				class="flex flex-col gap-2 max-h-[50vh] overflow-y-auto bg-secondary-800 dark:bg-space-950 p-2 rounded"
				role="list"
			>
				{#each candidates as c (key(c))}
					{@const note = cleanupNote(c, trunk)}
					<div class="flex flex-row items-start gap-2 text-sm" role="listitem">
						<Checkbox
							class="mt-1"
							checked={selected.has(key(c))}
							disabled={locked}
							on:change={onToggle(c)}
							aria-label="Select {c.name}"
						/>
						<div class="flex flex-col min-w-0 flex-1 gap-0.5">
							<div class="flex flex-row items-center gap-2 flex-wrap">
								<code class="text-white break-all">{c.name}</code>
								<span class="font-mono text-xs text-primary-400" title={c.sha}
									>{shortSha(c.sha)}</span
								>
							</div>
							<span class="text-xs text-gray-400 truncate" title={c.subject}>
								{c.subject} · {c.author} · {dateText(c.date)}
							</span>
							<span class="flex flex-row items-center gap-1 text-xs {toneClass(note.tone)}">
								{#if note.tone === 'safe'}
									<CheckCircleSolid class="w-3 h-3 shrink-0" />
								{:else if note.tone === 'unknown'}
									<QuestionCircleSolid class="w-3 h-3 shrink-0" />
								{:else}
									<ExclamationCircleSolid class="w-3 h-3 shrink-0" />
								{/if}
								{note.text}
							</span>
							{#if !c.safe && c.unmergedCommits.length > 0}
								<details class="text-xs text-gray-300">
									<summary class="cursor-pointer text-yellow-300">
										Show commits not on {trunk}
									</summary>
									<ul class="pl-4">
										{#each c.unmergedCommits as commit}
											<li class="truncate" title={commit.subject}>
												<span class="font-mono text-primary-400">{shortSha(commit.sha)}</span>
												{commit.subject}
											</li>
										{/each}
										{#if c.unmergedCommitCount > c.unmergedCommits.length}
											<li class="text-gray-400">
												…and {c.unmergedCommitCount - c.unmergedCommits.length} more
											</li>
										{/if}
									</ul>
								</details>
							{/if}
						</div>
						<Button
							size="xs"
							color="alternative"
							class="shrink-0"
							disabled={locked}
							on:click={reviewOne(c)}>Clean up</Button
						>
					</div>
				{/each}
			</div>
		{/if}
		<div class="flex flex-row-reverse gap-2 flex-wrap">
			<Button
				size="sm"
				disabled={locked || selectedCandidates.length === 0}
				on:click={reviewSelected}
			>
				Delete selected ({selectedCandidates.length})
			</Button>
			<Button
				size="sm"
				color="alternative"
				disabled={locked || mergedCandidates.length === 0}
				on:click={reviewAllMerged}
			>
				Clean up all merged ({mergedCandidates.length})
			</Button>
			<Button size="sm" color="alternative" disabled={deleting} on:click={close}>Close</Button>
		</div>
	{:else}
		<p class="text-xl text-primary-400">Delete {localBranchCount(confirming.length)}?</p>
		<p class="text-sm text-gray-300">
			These local branches will be deleted. Remote branches are not affected. Note the SHAs: a
			deleted branch can be recreated from its SHA.
		</p>
		<div
			class="flex flex-col gap-1 max-h-[40vh] overflow-y-auto bg-secondary-800 dark:bg-space-950 p-2 rounded text-sm"
			role="list"
		>
			{#each confirming as c (key(c))}
				{@const note = cleanupNote(c, trunk)}
				<div class="flex flex-row items-center gap-2" role="listitem">
					<code class="text-white break-all">{c.name}</code>
					<span class="font-mono text-xs text-primary-400 shrink-0" title={c.sha}
						>{shortSha(c.sha)}</span
					>
					<span class="text-xs {toneClass(note.tone)} truncate" title={note.text}>{note.text}</span>
				</div>
			{/each}
		</div>
		{#if needsAck}
			<div class="bg-yellow-900/20 border border-yellow-700 p-3 rounded flex flex-col gap-2">
				<div class="flex items-start gap-2">
					<ExclamationCircleSolid class="w-5 h-5 shrink-0 text-yellow-400" />
					<p class="text-sm text-yellow-300">
						{confirming.filter((c) => !c.safe).length} of these branches may hold commits that are not
						on {trunk}. After deletion they can only be recovered by SHA.
					</p>
				</div>
				<Label class="flex flex-row items-center gap-2 text-sm text-white dark:text-white">
					<Checkbox bind:checked={acknowledged} disabled={deleting} />
					I understand these commits will only be recoverable by SHA
				</Label>
			</div>
		{/if}
		<div class="flex flex-row-reverse gap-2">
			<Button
				size="sm"
				color={needsAck ? 'red' : 'primary'}
				disabled={!canDelete}
				on:click={runDelete}
			>
				{#if deleting}
					<Spinner size="4" class="me-2" />
				{/if}
				Delete {localBranchCount(confirming.length)}
			</Button>
			<Button size="sm" color="alternative" disabled={deleting} on:click={back}>Back</Button>
		</div>
	{/if}
</Modal>
