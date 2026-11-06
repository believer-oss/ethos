// Error toasts from background polls. A poll that keeps failing shows one toast, with the time it
// started failing, and the toast goes once the same poll succeeds again. Errors from a person's
// action stay until dismissed. No Tauri imports, so they can be tested on their own.

export interface ToastError {
	id: number;
	message: string;
	// The background poll that raised it; absent for errors from a person's action.
	source?: string;
}

/** The text of an error event's payload. */
export const errorText = (error: unknown): string => {
	if (typeof error === 'string') return error;
	const message = (error as { message?: unknown } | null)?.message;
	return typeof message === 'string' && message ? message : JSON.stringify(error);
};

/** "05:20": the local time, for "at …". */
export const clockTime = (at: Date): string =>
	`${String(at.getHours()).padStart(2, '0')}:${String(at.getMinutes()).padStart(2, '0')}`;

/** The toasts with a failed poll's error added, unless that poll already has one showing. */
export const withPollError = (
	errors: ToastError[],
	source: string,
	error: unknown,
	at: Date,
	id: number
): ToastError[] =>
	errors.some((e) => e.source === source)
		? errors
		: [...errors, { id, source, message: `${errorText(error)} (at ${clockTime(at)})` }];

/** The toasts without a poll's errors, once it succeeds again. */
export const withoutPollErrors = (errors: ToastError[], source: string): ToastError[] =>
	errors.some((e) => e.source === source) ? errors.filter((e) => e.source !== source) : errors;
