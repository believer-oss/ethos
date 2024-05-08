<script lang="ts">
	import { onMount } from 'svelte';
	import dayjs from 'dayjs';
	import duration from 'dayjs/plugin/duration';

	dayjs.extend(duration);

	export let from: string;
	export let onFinished: () => void = () => {};

	let remaining = {
		string: '0d 0h 0m 0s',
		years: 0,
		months: 0,
		weeks: 0,
		days: 0,
		hours: 0,
		minutes: 0,
		seconds: 0,

		done: true
	};

	let diff = 0;
	let target: dayjs.Dayjs;
	let local: dayjs.Dayjs;
	let timer: number;
	let r;

	const update = () => {
		if (diff > 0) {
			r = dayjs.duration(diff);

			let s = '';

			if (r.months() > 0) {
				s += `${r.months()}m `;
			}

			if (r.days() > 0) {
				s += `${r.days()}d `;
			}

			if (r.hours() > 0) {
				s += `${r.hours()}h `;
			}

			if (r.minutes() > 0) {
				s += `${r.minutes()}m `;
			}

			s += `${r.seconds()}s`;

			remaining = {
				string: s,
				years: r.years(),
				months: r.months(),
				weeks: r.weeks(),
				days: r.days(),
				hours: r.hours(),
				minutes: r.minutes(),
				seconds: r.seconds(),
				done: false
			};
			diff -= 1000;
		} else {
			remaining = {
				string: '0d 0h 0m 0s',
				years: 0,
				months: 0,
				weeks: 0,
				days: 0,
				hours: 0,
				minutes: 0,
				seconds: 0,
				done: true
			};
			clearInterval(timer);
			onFinished();
		}
	};

	onMount(() => {
		target = dayjs(from);

		if (dayjs.isDayjs(target)) {
			local = dayjs();
			diff = target.valueOf() - local.valueOf();
		}

		// initial update
		update();

		timer = setInterval(update, 1000);
	});
</script>

<slot {remaining} />
