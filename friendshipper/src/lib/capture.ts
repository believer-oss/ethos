import { invoke } from '@tauri-apps/api/core';
import type { CapturePendingSummary, CaptureStatus } from '$lib/types';

export const getCaptureStatus = async (): Promise<CaptureStatus> => invoke('get_capture_status');

export const getCapturePending = async (): Promise<CapturePendingSummary | null> =>
	invoke('get_capture_pending');

export const cancelCapture = async (sessionId: string): Promise<{ closed: number }> =>
	invoke('cancel_capture', { sessionId });
