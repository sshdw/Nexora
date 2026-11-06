'use client'

export type ActivityNodeStatus = 'pending' | 'active' | 'done'

export interface ActivityNode {
  id: string
  label: string
  status?: ActivityNodeStatus
}

interface ActivitySpineProps {
  nodes: ActivityNode[]
  /** Accessible label for the whole spine. */
  ariaLabel?: string
  /** Which node is currently active (overrides per-node statuses for the active look). */
  activeId?: string | null
  onSelect?: (id: string) => void
}

function statusOf(node: ActivityNode, activeId: string | null): ActivityNodeStatus {
  if (node.id === activeId) {
    return 'active'
  }
  return node.status ?? 'pending'
}

function nodeDot(status: ActivityNodeStatus): string {
  const base = 'w-2 h-2 rounded-full ring-1 transition-transform duration-200'
  if (status === 'active') {
    return `${base} bg-[#9AB8C8] ring-[#9AB8C8] scale-110`
  }
  if (status === 'done') {
    return `${base} bg-[#F3F3F4] ring-transparent`
  }
  return `${base} bg-[#18191C] border border-[#6D727A] ring-transparent`
}

function nodeText(status: ActivityNodeStatus): string {
  if (status === 'active') {
    return 'text-[#9AB8C8]'
  }
  if (status === 'done') {
    return 'text-[#F3F3F4]'
  }
  return 'text-[#A4A7AD]'
}

export default function ActivitySpine({
  nodes,
  ariaLabel = 'Agent Activity Spine',
  activeId = null,
  onSelect,
}: ActivitySpineProps) {
  return (
    <div
      className="flex max-w-[1180px] mx-auto flex-col gap-1 pt-2 pb-4"
      role="region"
      aria-label={ariaLabel}
    >
      <svg
        className="hidden lg:block w-px h-full mx-[10px] self-stretch"
        stroke="#303238"
        strokeWidth="1"
      >
        <line x1="50%" y1="0" x2="50%" y2="100%" />
      </svg>
      <div className="flex flex-col lg:flex-row gap-2 lg:gap-4 items-start lg:items-center">
        {nodes.map((node) => {
          const status = statusOf(node, activeId)
          const isClickable = typeof onSelect === 'function'
          return (
            <button
              key={node.id}
              type="button"
              aria-label={`${node.label}, ${status}`}
              data-state={status}
              onClick={() => onSelect?.(node.id)}
              disabled={!isClickable}
              className="flex w-full lg:w-auto flex-col items-start lg:items-center gap-1.5 rounded-md p-1.5 lg:p-2 text-left lg:text-center"
            >
              <span className={nodeDot(status)} aria-hidden="true" />
              <span className={`text-[11px] lg:text-xs leading-none font-medium ${nodeText(status)}`}>
                {node.label}
              </span>
            </button>
          )
        })}
      </div>
    </div>
  )
}
