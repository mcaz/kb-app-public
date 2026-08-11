import { Sprout } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";

import { Button } from "@/components/atoms/ui/button";
import { OnboardingLayout } from "@/components/templates/OnboardingLayout";
import { useOnboard } from "@/lib/queries";

/** 最初の vault を作る(FR-A1)。 */
export function OnboardingPage() {
  const { t } = useTranslation("onboarding");
  const onboard = useOnboard();

  return (
    <OnboardingLayout
      mark={<Icon as={Sprout} className="size-10" />}
      title={t("title")}
      lead={t("lead")}
    >
      <Button variant="primary" disabled={onboard.isPending} onClick={() => onboard.mutate()}>
        {t("start")}
      </Button>
    </OnboardingLayout>
  );
}
