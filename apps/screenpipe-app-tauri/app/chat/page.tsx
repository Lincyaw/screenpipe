// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com
"use client";

import { useEffect } from "react";
import { useRouter } from "next/navigation";

/** Old chat-window links now open the collected activity. */
export default function ChatPage() {
  const router = useRouter();
  useEffect(() => { router.replace("/home"); }, [router]);
  return null;
}
