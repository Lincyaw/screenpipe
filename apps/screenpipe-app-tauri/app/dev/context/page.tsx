// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com
"use client";

import HomePage from "@/app/(main)/home/page";
import MainLayout from "@/app/(main)/layout";

/** Browser-mock preview of the real data shell without native migration offers. */
export default function ContextPreview() {
  if (process.env.NEXT_PUBLIC_SCREENPIPE_WEB_DEV !== "mock") return null;
  return <MainLayout><HomePage /></MainLayout>;
}
