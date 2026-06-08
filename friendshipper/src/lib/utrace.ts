import { invoke } from '@tauri-apps/api/core';
import type { RecentTracesResponse, TraceEntry } from '$lib/types';

export const getUtraceDates = async (): Promise<string[]> => invoke('get_utrace_dates');

export const getRecentUtraces = async (
	limit?: number,
	before?: string | null
): Promise<RecentTracesResponse> => invoke('get_recent_utraces', { limit, before: before ?? null });

export const getUtracesForDate = async (date: string): Promise<TraceEntry[]> =>
	invoke('get_utraces_for_date', { date });

export const downloadUtrace = async (key: string, destPath: string): Promise<void> =>
	invoke('download_utrace', { key, destPath });

export const openUtraceInInsights = async (key: string): Promise<void> =>
	invoke('open_utrace_in_insights', { key });
