/* ============================================================
   LINEAGE — copied source, not an npm dependency (#218)

   Upstream      React Bits — TextType (TextAnimations)
   Repository    https://github.com/DavidHDev/react-bits
   Commit        496968c4117e21278f13917201b7d615bc5e7450
                 ("improve components", 2026-10-12)
   File          src/ts-tailwind/TextAnimations/TextType/TextType.tsx
   License       LICENSE.md — "MIT + Commons Clause License
                 Condition v1.0" (GitHub classifies it
                 NOASSERTION; © 2026 David Haz)

   Why copied: React Bits ships components as source, not as a
   package, so the stack vendors them. This one text-animation
   is the minimum that proves the pattern end to end: it owns a
   JS timeline (a setTimeout typing chain + a GSAP cursor-blink
   tween) and therefore the thing the Nexora reduced-motion gate
   has to reach.

   Nexora changes, all of them required by house rules:
     1. `import { useReducedMotion } from "@/lib/useReducedMotion"`
        + a hand-written reduced-motion branch that renders the
        final static frame and starts NO timeline (upstream ships
        none — see src/lib/motion.test.tsx), and an empty-text
        guard so `text={[]}` renders nothing instead of throwing.
     2. The React Server Components client directive dropped from
        the top of the file — this is a Vite app, not Next.
     3. The default cursor character kept; `cursorClassName` and
        Tailwind utility strings kept as authored so diffs
        against upstream stay legible.

   Not changed: the typing/deleting algorithm, its timing
   defaults, the reverse-mode handling, or the public prop
   surface. Any future change here is a Nexora decision, not a
   port.
   ============================================================ */
import {
  type ElementType,
  useEffect,
  useRef,
  useState,
  createElement,
  useMemo,
  useCallback,
} from 'react';
import { gsap } from 'gsap';

import { useReducedMotion } from '@/lib/useReducedMotion';

interface TextTypeProps {
  className?: string;
  showCursor?: boolean;
  hideCursorWhileTyping?: boolean;
  cursorCharacter?: string | React.ReactNode;
  cursorBlinkDuration?: number;
  cursorClassName?: string;
  text: string | string[];
  as?: ElementType;
  typingSpeed?: number;
  initialDelay?: number;
  pauseDuration?: number;
  deletingSpeed?: number;
  loop?: boolean;
  textColors?: string[];
  variableSpeed?: { min: number; max: number };
  onSentenceComplete?: (sentence: string, index: number) => void;
  startOnVisible?: boolean;
  reverseMode?: boolean;
}

const TextType = ({
  text,
  as: Component = 'div',
  typingSpeed = 50,
  initialDelay = 0,
  pauseDuration = 2000,
  deletingSpeed = 30,
  loop = true,
  className = '',
  showCursor = true,
  hideCursorWhileTyping = false,
  cursorCharacter = '|',
  cursorClassName = '',
  cursorBlinkDuration = 0.5,
  textColors = [],
  variableSpeed,
  onSentenceComplete,
  startOnVisible = false,
  reverseMode = false,
  ...props
}: TextTypeProps & React.HTMLAttributes<HTMLElement>) => {
  const [displayedText, setDisplayedText] = useState('');
  const [currentCharIndex, setCurrentCharIndex] = useState(0);
  const [isDeleting, setIsDeleting] = useState(false);
  const [currentTextIndex, setCurrentTextIndex] = useState(0);
  const [isVisible, setIsVisible] = useState(!startOnVisible);
  const cursorRef = useRef<HTMLSpanElement>(null);
  const containerRef = useRef<HTMLElement>(null);
  // NEXORA (#218): the hand-written reduced-motion branch. When this
  // is true the component renders the final static frame below and
  // no timeline runs — no GSAP tween, no typing/deleting chain.
  const reducedMotion = useReducedMotion();

  const textArray = useMemo(() => (Array.isArray(text) ? text : [text]), [text]);

  const getRandomSpeed = useCallback(() => {
    if (!variableSpeed) return typingSpeed;
    const { min, max } = variableSpeed;
    return Math.random() * (max - min) + min;
  }, [variableSpeed, typingSpeed]);

  const getCurrentTextColor = () => {
    if (textColors.length === 0) return 'inherit';
    return textColors[currentTextIndex % textColors.length];
  };

  useEffect(() => {
    if (!startOnVisible || !containerRef.current) return;

    const observer = new IntersectionObserver(
      entries => {
        entries.forEach(entry => {
          if (entry.isIntersecting) {
            setIsVisible(true);
          }
        });
      },
      { threshold: 0.1 }
    );

    observer.observe(containerRef.current);
    return () => observer.disconnect();
  }, [startOnVisible]);

  useEffect(() => {
    if (reducedMotion) return;
    if (showCursor && cursorRef.current) {
      gsap.set(cursorRef.current, { opacity: 1 });
      gsap.to(cursorRef.current, {
        opacity: 0,
        duration: cursorBlinkDuration,
        repeat: -1,
        yoyo: true,
        ease: 'power2.inOut'
      });
    }
  }, [reducedMotion, showCursor, cursorBlinkDuration]);

  useEffect(() => {
    // NEXORA (#218): reduced motion skips the typing/deleting chain
    // entirely. The static frame below carries the same information
    // the animation would have settled on (all text, fully
    // revealed), so nothing is lost for a user who opted out.
    // Same for an empty `text` array: there is no sentence to
    // read, and upstream would dereference `undefined` here.
    if (reducedMotion || !isVisible || textArray.length === 0) return;

    let timeout: ReturnType<typeof setTimeout>;

    const currentText = textArray[currentTextIndex];
    const processedText = reverseMode ? currentText.split('').reverse().join('') : currentText;

    const executeTypingAnimation = () => {
      if (isDeleting) {
        if (displayedText === '') {
          setIsDeleting(false);
          if (currentTextIndex === textArray.length - 1 && !loop) {
            return;
          }

          if (onSentenceComplete) {
            onSentenceComplete(textArray[currentTextIndex], currentTextIndex);
          }

          setCurrentTextIndex(prev => (prev + 1) % textArray.length);
          setCurrentCharIndex(0);
          timeout = setTimeout(() => {}, pauseDuration);
        } else {
          timeout = setTimeout(() => {
            setDisplayedText(prev => prev.slice(0, -1));
          }, deletingSpeed);
        }
      } else {
        if (currentCharIndex < processedText.length) {
          timeout = setTimeout(
            () => {
              setDisplayedText(prev => prev + processedText[currentCharIndex]);
              setCurrentCharIndex(prev => prev + 1);
            },
            variableSpeed ? getRandomSpeed() : typingSpeed
          );
        } else if (textArray.length >= 1) {
          if (!loop && currentTextIndex === textArray.length - 1) return;
          timeout = setTimeout(() => {
            setIsDeleting(true);
          }, pauseDuration);
        }
      }
    };

    if (currentCharIndex === 0 && !isDeleting && displayedText === '') {
      timeout = setTimeout(executeTypingAnimation, initialDelay);
    } else {
      executeTypingAnimation();
    }

    return () => clearTimeout(timeout);
  }, [
    reducedMotion,
    currentCharIndex,
    displayedText,
    isDeleting,
    typingSpeed,
    deletingSpeed,
    pauseDuration,
    textArray,
    currentTextIndex,
    loop,
    initialDelay,
    isVisible,
    reverseMode,
    variableSpeed,
    onSentenceComplete
  ]);

  // NEXORA (#218): no text, nothing to reveal. Upstream reads
  // `textArray[finalIndex]` unconditionally, so `text={[]}` threw
  // on the reduced-motion path (and on the cursor path whenever
  // `hideCursorWhileTyping` was set). Rendering nothing is the
  // honest answer: there is no frame to show and no sentence to
  // type. Guarded here — after every hook, so hook order is
  // untouched.
  if (textArray.length === 0) return null;

  const shouldHideCursor =
    hideCursorWhileTyping && (currentCharIndex < textArray[currentTextIndex].length || isDeleting);

  // NEXORA (#218): static end frame under reduced motion — the last
  // sentence fully typed, honouring reverseMode, and the same
  // colour the animation would have settled on. The cursor is a
  // live motion affordance, so it is omitted rather than frozen
  // mid-blink.
  const reducedFrame = reducedMotion
    ? (() => {
        const finalIndex = textArray.length - 1;
        const finalText = textArray[finalIndex];
        const shown = reverseMode ? finalText.split('').reverse().join('') : finalText;
        const color = textColors.length === 0 ? 'inherit' : textColors[finalIndex % textColors.length];
        return <span className="inline" style={{ color }}>{shown}</span>;
      })()
    : null;

  return createElement(
    Component,
    {
      ref: containerRef,
      className: `inline-block whitespace-pre-wrap tracking-tight ${className}`,
      ...props
    },
    reducedMotion ? (
      reducedFrame
    ) : (
      <>
        <span className="inline" style={{ color: getCurrentTextColor() || 'inherit' }}>
          {displayedText}
        </span>
        {showCursor && (
          <span
            ref={cursorRef}
            className={`ml-1 inline-block opacity-100 ${shouldHideCursor ? 'hidden' : ''} ${cursorClassName}`}
          >
            {cursorCharacter}
          </span>
        )}
      </>
    )
  );
};

export default TextType;
