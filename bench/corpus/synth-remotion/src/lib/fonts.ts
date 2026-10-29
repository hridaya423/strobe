import {continueRender, delayRender} from 'remotion';

export function loadFonts(faces: {family: string; url: string; weight: string}[]) {
  const handle = delayRender('fonts');
  Promise.all(
    faces.map(async ({family, url, weight}) => {
      const face = new FontFace(family, `url(${url})`, {weight});
      await face.load();
      document.fonts.add(face);
    }),
  ).then(() => continueRender(handle));
}
