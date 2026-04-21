<script lang="ts">
	import {
		Button,
		Modal,
		Spinner,
		Table,
		TableBody,
		TableBodyCell,
		TableBodyRow,
		TableHead,
		TableHeadCell,
		Tooltip
	} from 'flowbite-svelte';
	import { ClipboardOutline, GithubSolid } from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import type { CommitFileInfo } from '@ethos/core';
	import { getCommitInfo, getCommitFileTextClass, showCommitFiles } from '$lib/repo';
	import { repoStatus } from '$lib/stores';
	import { openUrl } from '$lib/utils';
	import type { CommitInfo } from '$lib/types';

	export let open: boolean = false;
	export let sha: string | null = null;

	let info: CommitInfo | null = null;
	let files: CommitFileInfo[] = [];
	let loading = false;
	let errorMessage = '';

	let lastLoadedSha: string | null = null;

	const load = async (target: string) => {
		loading = true;
		errorMessage = '';
		info = null;
		files = [];
		try {
			const [infoResult, filesResult] = await Promise.all([
				getCommitInfo(target),
				showCommitFiles(target, false).catch(() => [])
			]);
			info = infoResult;
			files = filesResult;
			lastLoadedSha = target;
		} catch (e) {
			errorMessage = String(e);
		} finally {
			loading = false;
		}
	};

	$: if (open && sha && sha !== lastLoadedSha) {
		void load(sha);
	}

	$: if (!open) {
		lastLoadedSha = null;
	}

	$: githubUrl =
		info && $repoStatus?.repoOwner && $repoStatus?.repoName
			? `https://github.com/${$repoStatus.repoOwner}/${$repoStatus.repoName}/commit/${info.sha}`
			: null;

	const copy = async (value: string, label: string) => {
		try {
			await navigator.clipboard.writeText(value);
			await emit('success', `${label} copied`);
		} catch {
			// ignore
		}
	};

	const formatDate = (iso: string): string => {
		const d = new Date(iso);
		if (Number.isNaN(d.getTime())) return iso;
		return d.toLocaleString();
	};

	const openOnGitHub = async () => {
		if (!githubUrl) return;
		try {
			await openUrl(githubUrl);
		} catch (e) {
			await emit('error', e);
		}
	};
</script>

<Modal
	bind:open
	size="xl"
	class="bg-secondary-700 dark:bg-space-900"
	bodyClass="!border-t-0 flex-1 overflow-y-auto overscroll-contain"
	backdropClass="fixed inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	dismissable
	outsideclose
>
	<svelte:fragment slot="header">
		<div class="flex items-center gap-2 w-full">
			<h3 class="text-primary-400 text-xl flex items-center gap-2">
				Commit
				{#if info}
					<span class="font-mono text-sm text-gray-300">{info.shortSha}</span>
				{/if}
			</h3>
		</div>
	</svelte:fragment>

	{#if loading}
		<div class="flex items-center gap-2 py-4">
			<Spinner size="5" />
			<span class="text-gray-300">Loading commit info…</span>
		</div>
	{:else if errorMessage}
		<p class="text-red-400 p-2">{errorMessage}</p>
	{:else if info}
		<div class="flex flex-col gap-4 text-sm">
			<!-- Metadata grid -->
			<div class="bg-secondary-800 dark:bg-space-950 p-3 rounded-md">
				<dl class="grid grid-cols-[auto,1fr] gap-x-4 gap-y-2">
					<dt class="text-gray-400">SHA</dt>
					<dd class="font-mono break-all flex items-center gap-2">
						<span class="text-gray-200">{info.sha}</span>
						<Button
							outline
							size="xs"
							class="p-1 border-0 focus-within:ring-0 dark:focus-within:ring-0"
							on:click={() => copy(info ? info.sha : '', 'SHA')}
						>
							<ClipboardOutline class="w-3 h-3" />
						</Button>
						<Tooltip class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
							>Copy full SHA</Tooltip
						>
					</dd>

					<dt class="text-gray-400">Author</dt>
					<dd class="text-gray-200">
						{info.authorName}
						<span class="text-gray-400">&lt;{info.authorEmail}&gt;</span>
					</dd>

					<dt class="text-gray-400">Authored</dt>
					<dd class="text-gray-200">{formatDate(info.authorDate)}</dd>

					{#if info.committerName !== info.authorName || info.committerEmail !== info.authorEmail}
						<dt class="text-gray-400">Committer</dt>
						<dd class="text-gray-200">
							{info.committerName}
							<span class="text-gray-400">&lt;{info.committerEmail}&gt;</span>
						</dd>
					{/if}

					{#if info.committerDate !== info.authorDate}
						<dt class="text-gray-400">Committed</dt>
						<dd class="text-gray-200">{formatDate(info.committerDate)}</dd>
					{/if}

					{#if info.parents.length > 0}
						<dt class="text-gray-400">
							{info.parents.length === 1 ? 'Parent' : 'Parents'}
						</dt>
						<dd class="font-mono text-gray-200 break-all">
							{info.parents.map((p) => p.substring(0, 8)).join(', ')}
						</dd>
					{/if}
				</dl>
			</div>

			<!-- Full message -->
			<div>
				<h4 class="text-gray-400 text-xs uppercase tracking-wide mb-1">Message</h4>
				<pre
					class="bg-secondary-800 dark:bg-space-950 p-3 rounded-md text-gray-200 font-sans whitespace-pre-wrap break-words text-sm">{info.message}</pre>
			</div>

			<!-- File list -->
			<div>
				<div class="flex items-center gap-2 mb-1">
					<h4 class="text-gray-400 text-xs uppercase tracking-wide">Files changed</h4>
					<span class="text-xs text-gray-500 font-italic">({files.length})</span>
				</div>
				<div class="max-h-96 overflow-y-auto bg-secondary-800 dark:bg-space-950 rounded-md">
					<Table color="custom" striped={true}>
						<TableHead class="w-full border-b-0 p-2 bg-secondary-900 dark:bg-space-950">
							<TableHeadCell class="p-1 w-8">Action</TableHeadCell>
							<TableHeadCell class="p-1">File</TableHeadCell>
						</TableHead>
						<TableBody>
							{#each files as f, i (f.file)}
								<TableBodyRow
									class="text-left border-b-0 {i % 2 === 0
										? 'bg-secondary-800 dark:bg-space-950'
										: 'bg-secondary-700 dark:bg-space-900'}"
								>
									<TableBodyCell class="p-1 font-mono font-medium">
										<span class={getCommitFileTextClass(f.action) || 'text-gray-200'}>
											{f.action}
										</span>
									</TableBodyCell>
									<TableBodyCell class="p-1 font-mono text-xs break-all">
										<span class={getCommitFileTextClass(f.action) || 'text-gray-200'}>
											{f.displayName || f.file}
										</span>
									</TableBodyCell>
								</TableBodyRow>
							{:else}
								<TableBodyRow class="text-center border-b-0 bg-secondary-700 dark:bg-space-900">
									<TableBodyCell class="p-2" colspan="2">
										<p class="text-gray-300">No file changes listed.</p>
									</TableBodyCell>
								</TableBodyRow>
							{/each}
						</TableBody>
					</Table>
				</div>
			</div>
		</div>
	{/if}

	<svelte:fragment slot="footer">
		<div class="flex justify-center w-full">
			<Button size="sm" color="primary" disabled={!githubUrl} on:click={openOnGitHub}>
				<GithubSolid class="w-4 h-4 mr-2" />
				Open on GitHub
			</Button>
			{#if !githubUrl && info}
				<Tooltip class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
					>Repo owner/name not available from status</Tooltip
				>
			{/if}
		</div>
	</svelte:fragment>
</Modal>
