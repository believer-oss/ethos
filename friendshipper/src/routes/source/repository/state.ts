import type { FileHistoryRevision, RepoDirectoryListing } from '$lib/types';

// Module-scope state persists across route navigations within the app lifetime,
// so leaving the Repository tab and coming back returns the user to the same folder,
// with visited-history stack and selected file intact.
export const repositoryViewState = {
	currentPath: '',
	visitedStack: [] as string[],
	listingCache: new Map<string, RepoDirectoryListing>(),
	selectedPath: null as string | null,
	revisions: [] as FileHistoryRevision[],
	displayName: ''
};
