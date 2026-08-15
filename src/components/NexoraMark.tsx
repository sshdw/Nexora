export interface NexoraMarkProps {
  className?: string;
  alt?: string;
  width?: number;
  height?: number;
}

export default function NexoraMark({
  className,
  alt = "Nexora",
  width = 32,
  height = 32,
}: NexoraMarkProps) {
  return (
    <img
      src="/logo.svg"
      alt={alt}
      className={className}
      width={width}
      height={height}
      loading="eager"
      decoding="async"
    />
  );
}


