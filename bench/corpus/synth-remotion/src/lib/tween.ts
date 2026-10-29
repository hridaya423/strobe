import {Easing, interpolate} from 'remotion';

export function tween(
  t: number,
  start: number,
  duration: number,
  from: number,
  to: number,
  easing: (x: number) => number = Easing.inOut(Easing.quad),
) {
  return interpolate(t, [start, start + duration], [from, to], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
    easing,
  });
}
