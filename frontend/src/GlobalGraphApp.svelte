<script lang="ts">
  import { onMount } from 'svelte'
  import type {
    GlobalMinimap,
    GlobalOverviewNode,
    GlobalOverviewScene,
    GraphHandle,
  } from 'gcrates'
  import {
    GlobalGraphRenderer,
    paintMinimapVolume,
    paintProjectedScene,
    pickProjectedNode,
    type MacroCameraState,
    type ProjectedSceneNode,
  } from './global-renderer'

  type ViewMode = 'global' | 'focus'

  const numberFormat = new Intl.NumberFormat('en-US')
  const zoomMin = 0.72
  const zoomMax = 24
  const seedCrates = ['tokio', 'serde', 'syn', 'bevy', 'wgpu', 'axum']

  let graphHandle: GraphHandle | null = null
  let renderer: GlobalGraphRenderer | null = null
  let stageCanvas: HTMLCanvasElement | null = null
  let overlayCanvas: HTMLCanvasElement | null = null
  let minimapCanvas: HTMLCanvasElement | null = null
  let stageElement: HTMLDivElement | null = null
  let minimap: GlobalMinimap | null = null
  let currentScene: GlobalOverviewScene | null = null
  let displayScene: GlobalOverviewScene | null = null
  let renderedScene: GlobalOverviewScene | null = null
  let projectedNodes: ProjectedSceneNode[] = []
  let query = ''
  let matches: string[] = []
  let status = 'Loading registry volume'
  let renderBackend: 'webgpu' | 'projection' = 'projection'
  let viewMode: ViewMode = 'global'
  let focusAnchor: string | null = null
  let packageCount = 0
  let dependencyCount = 0
  let visibleNodeCount = 0
  let visibleEdgeCount = 0
  let hoverNode: GlobalOverviewNode | null = null
  let selectedAnchor: string | null = null
  let selectedTitle = 'Registry dependency volume'
  let selectedSummary =
    'Search for a crate or click a sphere to inspect it. Drag to rotate, use WASD to move, and zoom to stream denser cube regions.'
  let activePointerId: number | null = null
  let lastPointer = { x: 0, y: 0 }
  let dragTravel = 0
  let sceneHandle = 0
  let renderHandle = 0
  let movementHandle = 0
  let lastMovementTime = 0
  let sceneTransition: {
    from: GlobalOverviewScene
    to: GlobalOverviewScene
    startedAt: number
    duration: number
  } | null = null
  const pressedKeys = new Set<string>()
  let camera: MacroCameraState = defaultCamera()
  let renderCamera: MacroCameraState = defaultCamera()

  function defaultCamera(): MacroCameraState {
    return {
      focusX: 0,
      focusY: 0,
      focusZ: 0,
      zoom: 1.1,
      rotX: 0.44,
      rotY: -0.78,
      rotZ: 0,
    }
  }

  function dependencyCamera(): MacroCameraState {
    return {
      focusX: 0,
      focusY: 0,
      focusZ: 0,
      zoom: 1.8,
      rotX: 0.32,
      rotY: -0.74,
      rotZ: 0,
    }
  }

  function formatCount(value: number) {
    return numberFormat.format(Math.round(value))
  }

  function compactCount(value: number) {
    if (value >= 1_000_000) {
      return `${(value / 1_000_000).toFixed(value >= 10_000_000 ? 0 : 1)}M`
    }
    if (value >= 1_000) {
      return `${(value / 1_000).toFixed(value >= 100_000 ? 0 : 1)}K`
    }
    return formatCount(value)
  }

  function clamp(value: number, min: number, max: number) {
    return Math.min(Math.max(value, min), max)
  }

  function lerp(from: number, to: number, t: number) {
    return from + (to - from) * t
  }

  function easeInOutCubic(t: number) {
    return t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2
  }

  function lerpColor(from: number[], to: number[], t: number) {
    return [
      lerp(from[0] ?? 0, to[0] ?? 0, t),
      lerp(from[1] ?? 0, to[1] ?? 0, t),
      lerp(from[2] ?? 0, to[2] ?? 0, t),
      lerp(from[3] ?? 1, to[3] ?? 1, t),
    ]
  }

  function sceneNodeKey(node: GlobalOverviewNode) {
    return node.anchorName
  }

  function sceneEdgeKey(scene: GlobalOverviewScene, edge: GlobalOverviewScene['edges'][number]) {
    const from = scene.nodes[edge.from]?.anchorName ?? String(edge.from)
    const to = scene.nodes[edge.to]?.anchorName ?? String(edge.to)
    return from < to ? `${from}\u0000${to}` : `${to}\u0000${from}`
  }

  function fadedNode(node: GlobalOverviewNode) {
    return {
      ...node,
      size: Math.max(node.size * 0.18, 0.002),
      color: [node.color[0], node.color[1], node.color[2], 0],
      accent: [node.accent[0], node.accent[1], node.accent[2], 0],
    }
  }

  function blendScenes(from: GlobalOverviewScene, to: GlobalOverviewScene, t: number): GlobalOverviewScene {
    const fromNodes = new Map(from.nodes.map((node) => [sceneNodeKey(node), node]))
    const toNodes = new Map(to.nodes.map((node) => [sceneNodeKey(node), node]))
    const orderedKeys = [
      ...to.nodes.map(sceneNodeKey),
      ...from.nodes.map(sceneNodeKey).filter((key) => !toNodes.has(key)),
    ]

    const nodes = orderedKeys.map((key) => {
      const source = fromNodes.get(key)
      const target = toNodes.get(key)
      const start = source ?? (target ? fadedNode(target) : null)
      const end = target ?? (source ? fadedNode(source) : null)
      if (!start || !end) {
        return null
      }
      return {
        kind: t < 0.5 ? start.kind : end.kind,
        anchorName: end.anchorName,
        title: t < 0.5 ? start.title : end.title,
        subtitle: t < 0.5 ? start.subtitle : end.subtitle,
        index: 0,
        x: lerp(start.x, end.x, t),
        y: lerp(start.y, end.y, t),
        z: lerp(start.z, end.z, t),
        size: lerp(start.size, end.size, t),
        count: Math.round(lerp(start.count, end.count, t)),
        downloads: Math.round(lerp(start.downloads, end.downloads, t)),
        dependencyCount: Math.round(lerp(start.dependencyCount, end.dependencyCount, t)),
        dependentCount: Math.round(lerp(start.dependentCount, end.dependentCount, t)),
        packageIndex: end.packageIndex ?? start.packageIndex,
        color: lerpColor(start.color, end.color, t),
        accent: lerpColor(start.accent, end.accent, t),
      }
    }).filter((node): node is GlobalOverviewNode => Boolean(node && (node.size > 0.002 || (node.color[3] ?? 0) > 0.04)))

    nodes.forEach((node, index) => {
      node.index = index
    })

    const nodeIndexByKey = new Map(nodes.map((node, index) => [sceneNodeKey(node), index]))
    const fromEdges = new Map(from.edges.map((edge) => [sceneEdgeKey(from, edge), edge]))
    const toEdges = new Map(to.edges.map((edge) => [sceneEdgeKey(to, edge), edge]))
    const edgeKeys = [...toEdges.keys(), ...Array.from(fromEdges.keys()).filter((key) => !toEdges.has(key))]

    const edges = edgeKeys.map((key) => {
      const [fromKey, toKey] = key.split('\u0000')
      const fromIndex = nodeIndexByKey.get(fromKey)
      const toIndex = nodeIndexByKey.get(toKey)
      if (fromIndex === undefined || toIndex === undefined) {
        return null
      }
      const source = fromEdges.get(key)
      const target = toEdges.get(key)
      const startWeight = source?.weight ?? 0
      const endWeight = target?.weight ?? 0
      const startColor = source?.color ?? [0, 0, 0, 0]
      const endColor = target?.color ?? [0, 0, 0, 0]
      const color = lerpColor(startColor, endColor, t)
      if ((color[3] ?? 0) < 0.03 && lerp(startWeight, endWeight, t) < 0.06) {
        return null
      }
      const fromNode = nodes[fromIndex]
      const toNode = nodes[toIndex]
      return {
        from: fromIndex,
        to: toIndex,
        x1: fromNode.x,
        y1: fromNode.y,
        z1: fromNode.z,
        x2: toNode.x,
        y2: toNode.y,
        z2: toNode.z,
        weight: lerp(startWeight, endWeight, t),
        color,
      }
    }).filter((edge): edge is GlobalOverviewScene['edges'][number] => Boolean(edge))

    return {
      level: t < 0.5 ? from.level : to.level,
      leafMode: t < 0.5 ? from.leafMode : to.leafMode,
      nodes,
      edges,
    }
  }

  function sceneTransitionDuration(from: GlobalOverviewScene, to: GlobalOverviewScene) {
    const nodeDelta = Math.abs(from.nodes.length - to.nodes.length)
    const edgeDelta = Math.abs(from.edges.length - to.edges.length)
    return clamp(180 + nodeDelta * 0.95 + edgeDelta * 0.2, 180, 320)
  }

  function stepRenderCamera() {
    const nextCamera = { ...renderCamera }
    let animating = false
    const keys: Array<keyof MacroCameraState> = ['focusX', 'focusY', 'focusZ', 'zoom', 'rotX', 'rotY', 'rotZ']
    for (const key of keys) {
      const currentValue = renderCamera[key]
      const targetValue = camera[key]
      const delta = targetValue - currentValue
      if (Math.abs(delta) < 0.0008) {
        nextCamera[key] = targetValue
        continue
      }
      nextCamera[key] = currentValue + delta * (key === 'zoom' ? 0.16 : 0.18)
      animating = true
    }
    renderCamera = nextCamera
    return animating
  }

  function materializeScene(now = performance.now()) {
    if (!currentScene) {
      displayScene = null
      sceneTransition = null
      return null
    }
    if (!sceneTransition) {
      displayScene = currentScene
      return currentScene
    }

    const rawProgress = (now - sceneTransition.startedAt) / Math.max(sceneTransition.duration, 1)
    if (rawProgress >= 1) {
      sceneTransition = null
      displayScene = currentScene
      return currentScene
    }

    const easedProgress = easeInOutCubic(clamp(rawProgress, 0, 1))
    displayScene = blendScenes(sceneTransition.from, sceneTransition.to, easedProgress)
    return displayScene
  }

  function viewportAspect() {
    const rect = stageElement?.getBoundingClientRect()
    if (!rect) {
      return 1
    }
    return rect.width / Math.max(rect.height, 1)
  }

  function pointerPoint(event: PointerEvent | WheelEvent) {
    const rect = stageElement?.getBoundingClientRect()
    if (!rect) {
      return null
    }
    return { x: event.clientX - rect.left, y: event.clientY - rect.top }
  }

  function nodeBudget(zoom: number) {
    return Math.round(144 + clamp((zoom - 1) * 20, 0, 140))
  }

  function edgeBudget(zoom: number) {
    return Math.round(240 + clamp((zoom - 1) * 42, 0, 320))
  }

  function refreshMatches() {
    if (!graphHandle) {
      matches = []
      return
    }
    matches = query ? Array.from(graphHandle.searchPrefix(query, 8)) : []
  }

  function scheduleSceneBuild() {
    if (sceneHandle) {
      cancelAnimationFrame(sceneHandle)
    }
    sceneHandle = requestAnimationFrame(() => {
      sceneHandle = 0
      rebuildScene()
    })
  }

  function scheduleRender() {
    if (renderHandle) {
      cancelAnimationFrame(renderHandle)
    }
    renderHandle = requestAnimationFrame(() => {
      renderHandle = 0
      renderCurrentScene()
    })
  }

  function rebuildScene() {
    if (!graphHandle) {
      return
    }

    let nextScene: GlobalOverviewScene
    try {
      nextScene =
        viewMode === 'focus' && focusAnchor
          ? (graphHandle.dependencyFocus(focusAnchor) as GlobalOverviewScene)
          : (graphHandle.globalOverview(
              camera.focusX,
              camera.focusY,
              camera.focusZ,
              camera.zoom,
              viewportAspect(),
              nodeBudget(camera.zoom),
              edgeBudget(camera.zoom),
            ) as GlobalOverviewScene)
    } catch (error) {
      status = error instanceof Error ? error.message : 'Failed to build the scene'
      return
    }

    const sourceScene = materializeScene() ?? currentScene ?? nextScene
    currentScene = nextScene
    sceneTransition =
      sourceScene === nextScene
        ? null
        : {
            from: sourceScene,
            to: nextScene,
            startedAt: performance.now(),
            duration: sceneTransitionDuration(sourceScene, nextScene),
          }
    status =
      viewMode === 'focus' && focusAnchor
        ? `${renderBackend === 'webgpu' ? 'WebGPU' : 'Projected canvas'} · dependency tree · ${focusAnchor}`
        : `${renderBackend === 'webgpu' ? 'WebGPU' : 'Projected canvas'} · cube query · zoom ${camera.zoom.toFixed(2)}`
    renderCurrentScene()
  }

  function renderCurrentScene() {
    const sceneToRender = materializeScene()
    if (!sceneToRender) {
      return
    }
    const cameraAnimating = stepRenderCamera()

    visibleNodeCount = sceneToRender.nodes.length
    visibleEdgeCount = sceneToRender.edges.length

    if (renderer) {
      if (renderedScene !== sceneToRender) {
        renderer.setScene(sceneToRender)
        renderedScene = sceneToRender
      }
      renderer.setCamera(renderCamera)
      renderer.render()
    }

    if (overlayCanvas) {
      projectedNodes = paintProjectedScene(overlayCanvas, sceneToRender, renderCamera, {
        hoverAnchor: hoverNode?.anchorName ?? null,
        selectedAnchor,
      })
    } else {
      projectedNodes = []
    }

    if (minimapCanvas) {
      paintMinimapVolume(minimapCanvas, minimap, renderCamera)
    }

    if (sceneTransition || cameraAnimating) {
      scheduleRender()
    }
  }

  function globalNodeSummary(node: GlobalOverviewNode) {
    return [
      node.subtitle,
      `${compactCount(node.downloads)} downloads`,
      `${formatCount(node.dependentCount)} dependents`,
      `${formatCount(node.dependencyCount)} direct dependencies`,
      node.kind === 'crate' ? 'Click to open the dependency tree.' : 'Zoom in or select a crate for a dependency tree.',
    ].join('\n')
  }

  function applyNodeDetails(node: GlobalOverviewNode | null, sticky: boolean) {
    if (!node) {
      if (!sticky) {
        selectedTitle = viewMode === 'focus' && focusAnchor ? focusAnchor : 'Registry dependency volume'
        selectedSummary =
          viewMode === 'focus' && focusAnchor
            ? 'Dependency tree view. Drag to rotate, use WASD to move, and click another crate to refocus.'
            : 'Search for a crate or click a sphere to inspect it. Drag to rotate, use WASD to move, and zoom to stream denser cube regions.'
      }
      return
    }

    if (sticky) {
      selectedAnchor = node.anchorName
    }

    selectedTitle = node.title
    if (graphHandle && node.kind === 'crate' && viewMode === 'focus') {
      selectedSummary = graphHandle.crateSummary(node.anchorName) ?? node.subtitle
      return
    }
    selectedSummary =
      node.kind === 'crate'
        ? globalNodeSummary(node)
        : [
            node.subtitle,
            `${formatCount(node.count)} represented crates`,
            `${compactCount(node.downloads)} aggregated downloads`,
            `${formatCount(node.dependentCount)} dependents`,
            `${formatCount(node.dependencyCount)} direct dependencies`,
          ].join('\n')
  }

  function updateHover(point: { x: number; y: number }) {
    const next = pickProjectedNode(projectedNodes, point.x, point.y)?.node ?? null
    if (next?.anchorName !== hoverNode?.anchorName) {
      hoverNode = next
      if (!selectedAnchor) {
        applyNodeDetails(hoverNode, false)
      }
      scheduleRender()
    }
  }

  function enterDependencyView(crateName: string) {
    if (!graphHandle) {
      return
    }

    try {
      focusAnchor = crateName
      viewMode = 'focus'
      selectedAnchor = crateName
      hoverNode = null
      selectedTitle = crateName
      selectedSummary =
        graphHandle.crateSummary(crateName) ??
        'Dependency tree view. Direct dependencies, secondary dependencies, and leaf third-order dependencies are rendered around the selected crate.'
      camera = dependencyCamera()
      scheduleSceneBuild()
      scheduleRender()
    } catch (error) {
      status = error instanceof Error ? error.message : 'Failed to open the dependency tree'
    }
  }

  function selectVisibleNode(node: GlobalOverviewNode) {
    if (node.kind === 'crate') {
      enterDependencyView(node.anchorName)
      return
    }

    selectedAnchor = node.anchorName
    camera = {
      ...camera,
      focusX: node.x,
      focusY: node.y,
      focusZ: node.z,
      zoom: Math.max(camera.zoom, 1.8),
    }
    applyNodeDetails(node, true)
    scheduleSceneBuild()
    scheduleRender()
  }

  function jumpToCrate(name: string) {
    if (!graphHandle) {
      return
    }

    query = name
    refreshMatches()
    matches = []
    enterDependencyView(name)
  }

  function resolveSearchTarget(input: string) {
    if (!graphHandle) {
      return null
    }

    const trimmed = input.trim()
    if (!trimmed) {
      return null
    }

    const exactMatch = matches.find((match) => match.toLowerCase() === trimmed.toLowerCase())
    if (exactMatch) {
      return exactMatch
    }

    const fallbackMatches = Array.from(graphHandle.searchPrefix(trimmed, 8))
    return fallbackMatches[0] ?? null
  }

  function handleSearchSubmit(event: SubmitEvent) {
    event.preventDefault()
    const target = resolveSearchTarget(query)
    if (!target) {
      status = `No crate matched "${query.trim()}"`
      return
    }
    jumpToCrate(target)
  }

  function clearSelection() {
    if (viewMode === 'focus') {
      resetView()
      return
    }
    selectedAnchor = null
    applyNodeDetails(hoverNode, false)
    scheduleRender()
  }

  function resetView() {
    viewMode = 'global'
    focusAnchor = null
    camera = defaultCamera()
    selectedAnchor = null
    hoverNode = null
    applyNodeDetails(null, false)
    scheduleSceneBuild()
    scheduleRender()
  }

  function handleQueryInput(event: Event) {
    query = (event.currentTarget as HTMLInputElement).value
    refreshMatches()
  }

  function rotateBy(deltaX: number, deltaY: number) {
    camera = {
      ...camera,
      rotY: camera.rotY - deltaX * 0.008,
      rotX: clamp(camera.rotX + deltaY * 0.008, -Math.PI * 0.48, Math.PI * 0.48),
    }
  }

  function handlePointerDown(event: PointerEvent) {
    const point = pointerPoint(event)
    if (!point) {
      return
    }
    activePointerId = event.pointerId
    lastPointer = point
    dragTravel = 0
    stageElement?.setPointerCapture(event.pointerId)
  }

  function handlePointerMove(event: PointerEvent) {
    const point = pointerPoint(event)
    if (!point) {
      return
    }

    if (activePointerId === event.pointerId) {
      const deltaX = point.x - lastPointer.x
      const deltaY = point.y - lastPointer.y
      dragTravel += Math.abs(deltaX) + Math.abs(deltaY)
      lastPointer = point
      rotateBy(deltaX, deltaY)
      scheduleRender()
      return
    }

    updateHover(point)
  }

  function handlePointerUp(event: PointerEvent) {
    const point = pointerPoint(event)
    if (activePointerId !== event.pointerId) {
      return
    }

    stageElement?.releasePointerCapture(event.pointerId)
    activePointerId = null
    if (!point) {
      return
    }

    updateHover(point)
    if (dragTravel <= 8 && hoverNode) {
      selectVisibleNode(hoverNode)
    }
  }

  function handlePointerLeave() {
    if (activePointerId === null && hoverNode) {
      hoverNode = null
      if (!selectedAnchor) {
        applyNodeDetails(null, false)
      }
      scheduleRender()
    }
  }

  function handlePointerCancel(event: PointerEvent) {
    if (activePointerId === event.pointerId) {
      activePointerId = null
    }
  }

  function handleWheel(event: WheelEvent) {
    const nextZoom = clamp(camera.zoom * Math.exp(-event.deltaY * 0.0011), zoomMin, zoomMax)
    camera = { ...camera, zoom: nextZoom }
    if (viewMode === 'global') {
      scheduleSceneBuild()
    }
    scheduleRender()
  }

  function trapSceneEvents(node: HTMLElement) {
    const stopSceneEvent = (event: Event) => {
      event.stopPropagation()
    }
    const eventTypes = [
      'click',
      'pointercancel',
      'pointerdown',
      'pointerleave',
      'pointermove',
      'pointerup',
      'wheel',
    ] as const

    for (const eventType of eventTypes) {
      node.addEventListener(eventType, stopSceneEvent)
    }

    return {
      destroy() {
        for (const eventType of eventTypes) {
          node.removeEventListener(eventType, stopSceneEvent)
        }
      },
    }
  }

  function rotateX(point: [number, number, number], angle: number): [number, number, number] {
    const c = Math.cos(angle)
    const s = Math.sin(angle)
    return [point[0], point[1] * c - point[2] * s, point[1] * s + point[2] * c]
  }

  function rotateY(point: [number, number, number], angle: number): [number, number, number] {
    const c = Math.cos(angle)
    const s = Math.sin(angle)
    return [point[0] * c + point[2] * s, point[1], -point[0] * s + point[2] * c]
  }

  function rotateZ(point: [number, number, number], angle: number): [number, number, number] {
    const c = Math.cos(angle)
    const s = Math.sin(angle)
    return [point[0] * c - point[1] * s, point[0] * s + point[1] * c, point[2]]
  }

  function normalize3(point: [number, number, number]): [number, number, number] {
    const length = Math.hypot(point[0], point[1], point[2])
    if (length <= 0.0001) {
      return [0, 0, 0]
    }
    return [point[0] / length, point[1] / length, point[2] / length]
  }

  function unrotate(point: [number, number, number]): [number, number, number] {
    let rotated = rotateZ(point, -camera.rotZ)
    rotated = rotateY(rotated, -camera.rotY)
    rotated = rotateX(rotated, -camera.rotX)
    return rotated
  }

  function moveCamera(deltaSeconds: number) {
    let intent: [number, number, number] = [0, 0, 0]
    if (pressedKeys.has('w')) {
      intent[2] -= 1
    }
    if (pressedKeys.has('s')) {
      intent[2] += 1
    }
    if (pressedKeys.has('a')) {
      intent[0] -= 1
    }
    if (pressedKeys.has('d')) {
      intent[0] += 1
    }
    if (pressedKeys.has('q')) {
      intent[1] -= 1
    }
    if (pressedKeys.has('e')) {
      intent[1] += 1
    }

    const local = normalize3(intent)
    if (local[0] === 0 && local[1] === 0 && local[2] === 0) {
      return
    }

    const world = normalize3(unrotate(local))
    const speed = 2.6 / Math.pow(Math.max(camera.zoom, 0.85), 0.58)
    camera = {
      ...camera,
      focusX: camera.focusX + world[0] * speed * deltaSeconds,
      focusY: camera.focusY + world[1] * speed * deltaSeconds,
      focusZ: camera.focusZ + world[2] * speed * deltaSeconds,
    }
    if (viewMode === 'global') {
      scheduleSceneBuild()
    }
    scheduleRender()
  }

  function handleKeyDown(event: KeyboardEvent) {
    const target = event.target as HTMLElement | null
    if (
      target instanceof HTMLInputElement ||
      target instanceof HTMLTextAreaElement ||
      target?.isContentEditable
    ) {
      return
    }

    const key = event.key.toLowerCase()
    if (!['w', 'a', 's', 'd', 'q', 'e'].includes(key)) {
      return
    }
    event.preventDefault()
    pressedKeys.add(key)
  }

  function handleKeyUp(event: KeyboardEvent) {
    const key = event.key.toLowerCase()
    if (!['w', 'a', 's', 'd', 'q', 'e'].includes(key)) {
      return
    }
    pressedKeys.delete(key)
  }

  function movementTick(timestamp: number) {
    if (lastMovementTime === 0) {
      lastMovementTime = timestamp
    }
    const deltaSeconds = Math.min((timestamp - lastMovementTime) / 1000, 0.05)
    lastMovementTime = timestamp
    if (pressedKeys.size > 0) {
      moveCamera(deltaSeconds)
    }
    movementHandle = requestAnimationFrame(movementTick)
  }

  async function boot() {
    try {
      const wasm = await import('gcrates')
      const wasmUrl = new URL('../../pkg/gcrates_bg.wasm', import.meta.url)
      await wasm.default(wasmUrl)

      const response = await fetch('/graph.gcr')
      if (!response.ok) {
        throw new Error(`graph.gcr request failed with ${response.status}`)
      }
      const bytes = new Uint8Array(await response.arrayBuffer())
      const handle = wasm.GraphHandle.fromBytes(bytes)
      graphHandle = handle as GraphHandle
      packageCount = graphHandle.packageCount
      dependencyCount = graphHandle.dependencyCount
      minimap = graphHandle.globalMinimap() as GlobalMinimap
      refreshMatches()

      if (stageCanvas) {
        try {
          renderer = await GlobalGraphRenderer.create(stageCanvas)
          renderBackend = 'webgpu'
        } catch {
          renderer = null
          renderBackend = 'projection'
        }
      }

      status = `${renderBackend === 'webgpu' ? 'WebGPU' : 'Projected canvas'} ready`
      rebuildScene()
    } catch (error) {
      status = error instanceof Error ? error.message : 'Failed to initialize the registry view'
    }
  }

  onMount(() => {
    const onResize = () => {
      renderer?.resize()
      scheduleSceneBuild()
    }

    window.addEventListener('keydown', handleKeyDown)
    window.addEventListener('keyup', handleKeyUp)
    window.addEventListener('resize', onResize)
    movementHandle = requestAnimationFrame(movementTick)
    void boot()

    return () => {
      window.removeEventListener('keydown', handleKeyDown)
      window.removeEventListener('keyup', handleKeyUp)
      window.removeEventListener('resize', onResize)
      if (sceneHandle) {
        cancelAnimationFrame(sceneHandle)
      }
      if (renderHandle) {
        cancelAnimationFrame(renderHandle)
      }
      if (movementHandle) {
        cancelAnimationFrame(movementHandle)
      }
    }
  })
</script>

<svelte:head>
  <title>gcrates 3D registry volume</title>
</svelte:head>

<div class="registry-shell">
  <div
    bind:this={stageElement}
    class:dragging={activePointerId !== null}
    class="registry-stage"
    role="application"
    aria-label="Interactive 3D crates.io dependency volume"
    on:pointerdown={handlePointerDown}
    on:pointermove={handlePointerMove}
    on:pointerup={handlePointerUp}
    on:pointerleave={handlePointerLeave}
    on:pointercancel={handlePointerCancel}
    on:wheel|preventDefault={handleWheel}
  >
    <canvas bind:this={stageCanvas} class="registry-canvas"></canvas>
    <canvas bind:this={overlayCanvas} class="registry-overlay"></canvas>

    <section use:trapSceneEvents class="registry-toolbar">
      <div class="toolbar-mark">gcrates</div>
      <form class="toolbar-search" on:submit={handleSearchSubmit}>
        <span>Search crate</span>
        <div class="toolbar-search-row">
          <input
            type="search"
            value={query}
            placeholder="tokio, serde, bevy..."
            on:input={handleQueryInput}
          />
          <button type="submit" class="toolbar-search-submit">Go</button>
        </div>
      </form>
      {#if matches.length > 0}
        <div class="toolbar-matches">
          {#each matches as match}
            <button type="button" on:click={() => jumpToCrate(match)}>{match}</button>
          {/each}
        </div>
      {:else}
        <div class="toolbar-matches toolbar-matches--seed">
          {#each seedCrates as seed}
            <button type="button" on:click={() => jumpToCrate(seed)}>{seed}</button>
          {/each}
        </div>
      {/if}
      <p class="toolbar-help">
        {#if viewMode === 'focus'}
          Drag to rotate. Use WASD to move. Q / E move vertically. Wheel zooms the dependency tree. Click a crate to refocus.
        {:else}
          Drag to rotate. Use WASD to move. Q / E move vertically. Wheel zooms. Click a sphere to inspect it.
        {/if}
      </p>
    </section>

    {#if viewMode === 'global'}
      <section class="registry-minimap" aria-label="Registry volume minimap">
        <div class="detail-topline">Registry atlas</div>
        <canvas bind:this={minimapCanvas} class="registry-minimap-canvas"></canvas>
        <div class="registry-minimap-stats">
          <span>{formatCount(packageCount)} crates</span>
          <span>{formatCount(dependencyCount)} dependencies</span>
          <span>{visibleNodeCount} visible nodes</span>
          <span>{visibleEdgeCount} visible links</span>
        </div>
        <p class="registry-minimap-caption">{status}</p>
      </section>
    {/if}

    <aside use:trapSceneEvents class="registry-detail">
      <div class="detail-topline">
        {#if viewMode === 'focus'}
          Dependency tree
        {:else}
          {selectedAnchor ? 'Selected crate' : hoverNode ? 'Hover preview' : 'Registry volume'}
        {/if}
      </div>
      <h1>{selectedTitle}</h1>
      <pre>{selectedSummary}</pre>
      <div class="detail-actions">
        {#if selectedAnchor && viewMode === 'global'}
          <button type="button" on:click={clearSelection}>Clear selection</button>
        {/if}
        <button type="button" class="primary" on:click={resetView}>
          {viewMode === 'focus' ? 'Back to global' : 'Reset view'}
        </button>
      </div>
    </aside>
  </div>
</div>
