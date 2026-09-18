<script lang="ts">
	import { Button, Spinner } from 'flowbite-svelte';
	import { ExclamationCircleSolid } from 'flowbite-svelte-icons';
	import { emit, listen } from '@tauri-apps/api/event';
	import { onMount } from 'svelte';
	import type { SyncEvent } from '@ethos/core';
	import { forceDownloadDlls, forceDownloadEngine, getArtifactStatus } from '$lib/repo';
	import type { ArtifactStatus, SyncKind } from '$lib/types';

	/**
	 * Standing problems with what is installed, not events.
	 *
	 * Being on the wrong engine does not stop being true because you looked away, so these
	 * persist until fixed rather than appearing and fading. Written as a list because the
	 * next one to go here - a commit on your branch that will change the engine when you
	 * pull it - is a different warning in the same place.
	 */
	interface Warning {
		kind: SyncKind;
		text: string;
		detail: string;
		/** Whether this warning has an action, as opposed to being informational. */
		fixable: boolean;
	}

	const labels: Record<SyncKind, string> = {
		client: 'game client',
		engine: 'engine',
		editorDlls: 'editor binaries'
	};

	let warnings: Warning[] = [];
	let fixing: SyncKind | null = null;

	const toWarning = (status: ArtifactStatus): Warning | null => {
		// Only "installed, but not what this checkout needs" is a standing problem. Never
		// having synced is not a problem, it is a starting state; and the client has no
		// expected version to be wrong about.
		if (status.state !== 'outOfDate') return null;

		return {
			kind: status.kind,
			text: `Your ${labels[status.kind]} does not match this branch`,
			detail: `On disk: ${status.installed ?? 'unknown'} · This checkout needs: ${
				status.expected ?? 'unknown'
			}`,
			fixable: status.kind !== 'client'
		};
	};

	const refresh = async () => {
		try {
			warnings = (await getArtifactStatus())
				.map(toWarning)
				.filter((warning): warning is Warning => warning !== null);
		} catch {
			// The banner is a convenience; failing to load it must not put an error in
			// front of someone who did not ask for it. Diagnostics reports properly.
			warnings = [];
		}
	};

	const handleFix = async (kind: SyncKind) => {
		fixing = kind;
		try {
			if (kind === 'engine') {
				await forceDownloadEngine();
			} else if (kind === 'editorDlls') {
				await forceDownloadDlls();
			}
		} catch (e) {
			await emit('error', e);
		} finally {
			fixing = null;
			await refresh();
		}
	};

	onMount(() => {
		void refresh();
	});

	// A sync is the thing that changes the answer, so re-check when one ends. This is also
	// what catches the common case: the sync that installed a new engine failed, and the
	// banner appears without the user having to go looking for why the editor will not open.
	//
	// Deliberately not `finished`, which only means the transfer ended. The editor binaries
	// are copied out of staging after that, and the ledger still names the previous build
	// until the copy lands - refreshing there would raise an out-of-date banner for the
	// version being installed, and nothing would come along to take it back down.
	void listen<SyncEvent>('sync-event', (event) => {
		const { type } = event.payload;
		if (type !== 'installed' && type !== 'failed' && type !== 'cancelled') return;
		void refresh();
	});
</script>

{#each warnings as warning (warning.kind)}
	<div
		class="flex gap-2 items-center bg-yellow-900/70 h-6 max-h-6 w-full py-1 px-2 z-50 border-t border-yellow-700"
	>
		<ExclamationCircleSolid class="h-3 w-3 text-yellow-300 shrink-0" />
		<code class="text-xs text-yellow-100 text-nowrap">{warning.text}</code>
		<code class="text-xs text-yellow-200/70 text-nowrap truncate">{warning.detail}</code>
		<div class="w-full" />
		{#if warning.fixable}
			<Button
				outline
				color="dark"
				size="xs"
				title="Download the version this checkout needs"
				class="p-1 my-1 text-yellow-100 border-0 hover:bg-yellow-800 focus-within:ring-0 dark:focus-within:ring-0"
				disabled={fixing !== null}
				on:click={() => handleFix(warning.kind)}
			>
				{#if fixing === warning.kind}
					<Spinner size="3" />
				{:else}
					Sync now
				{/if}
			</Button>
		{/if}
	</div>
{/each}
