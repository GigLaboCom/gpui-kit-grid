import { defineCollection } from 'astro:content';
import { z } from 'astro/zod';
import { docsLoader } from '@astrojs/starlight/loaders';
import { docsSchema } from '@astrojs/starlight/schema';

export const collections = {
  docs: defineCollection({
    loader: docsLoader(),
    schema: docsSchema({
      extend: z.object({
        // An outline of a page still to be written: `src/routeData.ts` marks
        // it and keeps it out of search and search engines.
        stub: z.boolean().default(false),
      }),
    }),
  }),
};
