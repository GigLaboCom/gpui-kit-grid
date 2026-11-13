// The banner's copy. The site is English only.
const en: Record<string, string> = {
	banner_title: 'Cookie settings',
	banner_text:
		'We use Google Analytics to count visits and see which pages and demos are read. Until you accept, it runs without cookies. You choose what to allow.',
	accept_all: 'Accept all',
	reject_all: 'Reject all',
	customize: 'Customize',
	save_preferences: 'Save preferences',
	policy_link: 'Privacy',
	always_active: 'Always active',
	cat_essential: 'Essential',
	cat_essential_desc: 'Your cookie choice itself. Cannot be turned off.',
	cat_analytics: 'Analytics',
	cat_analytics_desc: 'Google Analytics cookies, to tell visits apart.',
	cat_marketing: 'Marketing',
	cat_marketing_desc: 'Advertising. The site uses none; your choice is kept in case that changes.',
};

export function t(key: string): string {
	return en[key] ?? key;
}
