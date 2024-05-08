<script lang="ts">
	import { FBXLoader } from 'three/examples/jsm/loaders/FBXLoader.js';
	import { T, useLoader } from '@threlte/core';
	import { OrbitControls, Text } from '@threlte/extras';

	const text = 'Baking pizza...';

	const fbx = useLoader(FBXLoader).load('/assets/PIZZA.fbx');
</script>

{#await fbx}
	<Text {text} position={[-6.5, 1, 0]} fontSize={2} characters="foo" color="white" />
{:then $fbx}
	<T is={$fbx}>
		<T.PerspectiveCamera
			makeDefault
			position={[80, 100, -100]}
			on:create={({ ref }) => {
				ref.lookAt(0, 1, 0);
			}}
		>
			<OrbitControls rotateSpeed={0.1} enableZoom={false} enablePan={false} />
		</T.PerspectiveCamera>
		<T.DirectionalLight position={[1.6, 2.8, 2.0]} intensity={5} />
		<T.AmbientLight />

		<T.GridHelper position={[0.0, -60.0, 0.0]} args={[600, 8, 0x555555, 0x555555]} />
	</T>
{/await}
