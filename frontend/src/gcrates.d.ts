declare module 'gcrates' {
  export interface FocalNode {
    index: number
    name: string
    role: string
    x: number
    y: number
    radius: number
    color: number[]
    accent: number[]
    downloads: number
    dependencyCount: number
  }

  export interface FocalEdge {
    from: number | null
    to: number | null
    x1: number
    y1: number
    x2: number
    y2: number
    color: number[]
    width: number
  }

  export interface FocalScene {
    viewportAspect: number
    nodes: FocalNode[]
    edges: FocalEdge[]
  }

  export interface GlobalOverviewNode {
    kind: 'cluster' | 'crate'
    anchorName: string
    title: string
    subtitle: string
    index: number
    x: number
    y: number
    z: number
    size: number
    count: number
    downloads: number
    dependencyCount: number
    dependentCount: number
    packageIndex: number | null
    color: number[]
    accent: number[]
  }

  export interface GlobalOverviewEdge {
    from: number
    to: number
    x1: number
    y1: number
    z1: number
    x2: number
    y2: number
    z2: number
    weight: number
    color: number[]
  }

  export interface GlobalOverviewScene {
    level: number
    leafMode: boolean
    nodes: GlobalOverviewNode[]
    edges: GlobalOverviewEdge[]
  }

  export interface GlobalMinimapVoxel {
    x: number
    y: number
    z: number
    count: number
    downloads: number
    dependencyCount: number
    dependentCount: number
    color: number[]
  }

  export interface GlobalMinimap {
    span: number
    grid: number
    voxels: GlobalMinimapVoxel[]
  }

  export interface GlobalCratePosition {
    name: string
    x: number
    y: number
    z: number
    rank: number
    downloads: number
    dependencyCount: number
    dependentCount: number
  }

  export default function init(
    input?:
      | RequestInfo
      | URL
      | Response
      | BufferSource
      | WebAssembly.Module
      | Promise<Response>,
  ): Promise<void>

  export class GraphHandle {
    static fromBytes(bytes: Uint8Array): GraphHandle
    readonly packageCount: number
    readonly dependencyCount: number
    searchPrefix(query: string, limit: number): string[]
    crateSummary(name: string): string | undefined
    focalScene(name: string, viewportAspect: number): FocalScene
    globalOverview(
      centerX: number,
      centerY: number,
      centerZ: number,
      zoom: number,
      viewportAspect: number,
      maxNodes: number,
      maxEdges: number,
    ): GlobalOverviewScene
    globalMinimap(): GlobalMinimap
    dependencyFocus(name: string): GlobalOverviewScene
    globalCratePosition(name: string): GlobalCratePosition
  }

  export class WasmRenderer {
    static create(canvasId: string): Promise<WasmRenderer>
    setFocus(graph: GraphHandle, crateName: string): void
    panBy(deltaX: number, deltaY: number): void
    zoomAt(factor: number, x: number, y: number): void
    hoverAt(x: number, y: number): FocalNode | null
    clearHover(): void
    resetView(): void
    resize(): void
    render(): void
  }
}
