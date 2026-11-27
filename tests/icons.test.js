const root = new URL('../', import.meta.url);

Deno.test('symbolic icons match the QML paths and ink boxes', () => {
    const qml = Deno.readTextFileSync(new URL('AirPodsIcon.qml', root));
    // Sample input: readonly property var ink: isPro ? [0.25, 26.25, 37.5, 25.75].
    const ink = qml.split('readonly property var ink:')[1].split('readonly property real fit:')[0];
    const frames = [...ink.matchAll(/\[([\d., ]+)\]/g)];
    for (const [index, model] of ['pro', 'max', 'buds'].entries()) {
        const svg = Deno.readTextFileSync(new URL(`packaging/icons/pods-${model}-symbolic.svg`, root));
        // Sample input: readonly property string proPath: "m12.8732 39.9868...".
        const path = qml.match(new RegExp(`property string ${model}Path: "([^"]+)"`))[1];
        const viewBox = frames[index][1].replaceAll(', ', ' ');
        if (
            !svg.includes('width="16" height="16"') ||
            !svg.includes('<style id="current-color-scheme">.ColorScheme-Text { color: #2e3436; }</style>') ||
            !svg.includes(`<path class="ColorScheme-Text" fill="currentColor" d="${path}"/>`) ||
            !svg.includes(`viewBox="${viewBox}"`)
        )
            throw new Error(`The ${model} SVG differs from AirPodsIcon.qml`);
    }
});
